//! How long the actions a profile recorded took, placed on the nodes of a
//! graph.

use crate::status::Landings;
use bviz_graph_layout::{Graph, Id, Kind, Names};
use bviz_invocation::{Spent, Timings};
use std::num::NonZeroU32;
use std::time::Duration;

/// Why a graph's nodes carry no action time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Untimed {
    /// No profile of the graph's invocation is open.
    NoProfile,
    /// The profile records no actions.
    NoActions,
    /// The profile names its actions by neither primary output nor owner.
    Unnamed,
    /// None of the actions the profile names is a node of the graph or
    /// owned by one.
    Unplaced,
}

/// Of the actions a profile recorded, how many land on a node of a graph.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Coverage {
    pub(crate) placed: u32,
    pub(crate) profiled: u32,
}

/// The time spent on one node.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Timed {
    /// The actions landing on the node itself: an action node's own, and on
    /// a target those of its actions the graph holds no node for.
    pub(crate) own: Spent,
    /// `own` and the actions landing on every node the node contains.
    pub(crate) held: Spent,
}

/// The time every shorter one is placed as on a log scale: an action that
/// short is all but instant, and a scale reaching down toward no time at all
/// would crowd every other time against its top.
const SHORTEST_PLACED: Duration = Duration::from_millis(1);

/// The least and the most time a node of one kind holds, of those that hold
/// any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Extent {
    least: Duration,
    most: Duration,
}

impl Extent {
    fn of(wall: Duration) -> Self {
        Extent {
            least: wall,
            most: wall,
        }
    }

    fn widen(&mut self, wall: Duration) {
        self.least = self.least.min(wall);
        self.most = self.most.max(wall);
    }

    /// Where `wall` stands in the extent on a log scale, from 0 at its least
    /// to 1 at its most, clamped to that range, with every time shorter than
    /// [`SHORTEST_PLACED`] placed as that. An extent of one time places it at
    /// its most.
    pub(crate) fn place(self, wall: Duration) -> f32 {
        let log = |wall: Duration| wall.max(SHORTEST_PLACED).as_secs_f64().ln();
        let span = log(self.most) - log(self.least);
        if span <= 0.0 {
            return 1.0;
        }
        ((log(wall) - log(self.least)) / span).clamp(0.0, 1.0) as f32
    }
}

/// A node's place in [`NodeTimes::timed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Slot(NonZeroU32);

impl Slot {
    /// The slot of `timed[index]`. Only nodes are timed, so `index` is
    /// below the node ids, which fit a `u32`.
    fn at(index: usize) -> Self {
        Slot(NonZeroU32::MIN.saturating_add(index as u32))
    }

    fn index(self) -> usize {
        self.0.get() as usize - 1
    }
}

/// The time spent on every node of one graph. Only the nodes an action lands
/// on and those containing them are timed, so a node's time is held apart
/// from the dense table finding it.
#[derive(Debug, PartialEq)]
pub(crate) struct NodeTimes {
    /// By node id, where in `timed` the node's time is.
    slots: Vec<Option<Slot>>,
    timed: Vec<Timed>,
    /// By node kind, the extent of the time its nodes hold.
    extents: Vec<(Kind, Extent)>,
    coverage: Coverage,
}

/// Where in `graph`, which `names` names, the actions `timings` recorded
/// land, sized for each owner among them to be looked up: one keying actions
/// known by no primary output, or one owning an action the graph holds no
/// node for.
pub(crate) fn landings<'a>(graph: &Graph, names: &'a Names, timings: &Timings) -> Landings<'a> {
    let owners = timings.actions().len() + timings.by_owner().count();
    Landings::new(graph, names, owners)
}

impl NodeTimes {
    /// Places what `timings` recorded on the nodes of `graph` that `landings`
    /// lands reports on.
    pub(crate) fn resolve<'a>(
        landings: &Landings<'a>,
        graph: &Graph,
        timings: &'a Timings,
    ) -> Result<Self, Untimed> {
        if timings.is_empty() {
            return Err(Untimed::NoActions);
        }
        if !timings.names_primary_outputs() && !timings.names_owners() {
            return Err(Untimed::Unnamed);
        }
        let landing = timings.actions().len() + timings.by_owner().count();
        let mut landed = Landed {
            slots: vec![None; landings.ids()],
            timed: Vec::with_capacity(landing),
        };
        let mut coverage = Coverage {
            placed: 0,
            profiled: timings.spent().total().actions,
        };
        for (primary_output, owner, timing) in timings.actions() {
            if let Some(node) = landings.action(primary_output, owner) {
                landed.timed(node).own.add_action(timing);
                coverage.placed += 1;
            }
        }
        for (owner, spent) in timings.by_owner() {
            if let Some(node) = landings.landing(owner).node() {
                landed.timed(node).own.add(spent);
                coverage.placed += spent.total().actions;
            }
        }
        if coverage.placed == 0 {
            return Err(Untimed::Unplaced);
        }
        let (slots, timed) = landed.roll_up(graph);
        let mut times = NodeTimes {
            slots,
            timed,
            extents: Vec::new(),
            coverage,
        };
        times.extents = times.measure(graph);
        Ok(times)
    }

    /// The extent of each kind's time over the timed nodes of `graph`.
    fn measure(&self, graph: &Graph) -> Vec<(Kind, Extent)> {
        let mut extents: Vec<(Kind, Extent)> = Vec::new();
        for node in &graph.nodes {
            let Some(timed) = self.get(node.id) else {
                continue;
            };
            let wall = timed.held.total().wall;
            match extents.iter_mut().find(|(kind, _)| *kind == node.kind) {
                Some((_, extent)) => extent.widen(wall),
                None => extents.push((node.kind, Extent::of(wall))),
            }
        }
        extents
    }

    /// The extent of the time the nodes of `kind` hold, absent where none of
    /// them holds any.
    pub(crate) fn extent(&self, kind: Kind) -> Option<Extent> {
        self.extents
            .iter()
            .find(|(of, _)| *of == kind)
            .map(|(_, extent)| *extent)
    }

    /// The time spent on the node `id` names, absent where no action lands on
    /// it or on anything it contains. An id past the graph's own, as a merge
    /// group's is, reads as an untimed node.
    pub(crate) fn get(&self, id: Id) -> Option<&Timed> {
        let slot = (*self.slots.get(id.0 as usize)?)?;
        Some(&self.timed[slot.index()])
    }

    pub(crate) fn coverage(&self) -> Coverage {
        self.coverage
    }
}

/// The time landing on the nodes of a graph, each node timed only with what
/// lands on it itself.
struct Landed {
    /// By node id, where in `timed` the node's time is.
    slots: Vec<Option<Slot>>,
    timed: Vec<Timed>,
}

impl Landed {
    /// The time of `node`, timed from now on if it was not.
    fn timed(&mut self, node: Id) -> &mut Timed {
        let slot = &mut self.slots[node.0 as usize];
        let slot = *slot.get_or_insert_with(|| {
            self.timed.push(Timed::default());
            Slot::at(self.timed.len() - 1)
        });
        &mut self.timed[slot.index()]
    }

    /// Adds each node's own time to its own held time and to that of every
    /// node of `graph` containing it, timing those containers.
    fn roll_up(mut self, graph: &Graph) -> (Vec<Option<Slot>>, Vec<Timed>) {
        let mut parent = vec![None; self.slots.len()];
        for node in &graph.nodes {
            parent[node.id.0 as usize] = node.parent;
        }
        let mut owners: Vec<Id> = Vec::with_capacity(self.timed.len());
        for (id, slot) in self.slots.iter().enumerate() {
            if slot.is_some() {
                owners.push(Id(id as u32));
            }
        }
        self.timed
            .reserve(untimed_containers(&owners, &parent, &self.slots));
        for owner in owners {
            let own = self.timed(owner).own.clone();
            let mut at = Some(owner);
            while let Some(node) = at {
                self.timed(node).held.add(&own);
                at = parent[node.0 as usize];
            }
        }
        (self.slots, self.timed)
    }
}

/// How many nodes containing one of `owners`, by `parent`, have no slot.
fn untimed_containers(owners: &[Id], parent: &[Option<Id>], slots: &[Option<Slot>]) -> usize {
    let mut counted = vec![false; slots.len()];
    let mut untimed = 0;
    for owner in owners {
        let mut at = parent[owner.0 as usize];
        while let Some(node) = at {
            let index = node.0 as usize;
            // A container with a slot is an owner, and counts its own.
            if slots[index].is_some() || counted[index] {
                break;
            }
            counted[index] = true;
            untimed += 1;
            at = parent[index];
        }
    }
    untimed
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_graph_layout::graph::Node;
    use bviz_graph_layout::{Kind, Names};
    use bviz_invocation::{ConfiguredTarget, Profiled, Runner, Share, Spawns, Timing};
    use std::time::Duration;

    const CONFIG: &str = "a7a71f";

    #[derive(Default)]
    struct Built {
        graph: Graph,
        names: Names,
    }

    impl Built {
        fn node(&mut self, name: &str, kind: Kind, parent: Option<Id>) -> Id {
            let id = self.names.intern(name);
            self.graph.add_node(Node::new(id, name, kind, parent));
            id
        }

        /// The package `//{package}` with the target `name` in it, configured
        /// under [`CONFIG`]: the package, the target and the configured target.
        fn target(&mut self, package: &str, name: &str) -> (Id, Id, Id) {
            let label = format!("//{package}:{name}");
            let package = match self.names.get(&format!("pkg:{package}")) {
                Some(package) => package,
                None => self.node(&format!("pkg:{package}"), Kind::Package, None),
            };
            let target = self.node(&format!("target:{label}"), Kind::Target, Some(package));
            let configured = self.node(
                &format!("ct:{label}#{CONFIG}"),
                Kind::ConfiguredTarget,
                Some(target),
            );
            (package, target, configured)
        }

        fn action(&mut self, primary_output: &str, owner: Id) -> Id {
            self.node(
                &format!("action:{primary_output}"),
                Kind::Action,
                Some(owner),
            )
        }

        fn resolve(&self, timings: &Timings) -> Result<NodeTimes, Untimed> {
            NodeTimes::resolve(
                &landings(&self.graph, &self.names, timings),
                &self.graph,
                timings,
            )
        }
    }

    fn timing(millis: u64, runner: Runner) -> Timing {
        Timing {
            wall: Duration::from_millis(millis),
            runner,
        }
    }

    fn owner(label: &str) -> Option<ConfiguredTarget<&str>> {
        Some(ConfiguredTarget::new(label, Some(CONFIG)))
    }

    /// An action as a profile records it: by primary output, owner and time.
    type Recorded<'a> = (Option<&'a str>, Option<ConfiguredTarget<&'a str>>, Timing);

    fn timings(actions: &[Recorded]) -> Timings {
        let mut timings = Timings::with_capacity(actions.len());
        for &(primary_output, owner, timing) in actions {
            timings.record(Profiled {
                primary_output,
                owner,
                wall: timing.wall,
                spawns: Spawns::on(timing.runner, timing.wall),
            });
        }
        timings
    }

    fn share(actions: u32, millis: u64) -> Share {
        Share {
            actions,
            wall: Duration::from_millis(millis),
        }
    }

    fn held(times: &NodeTimes, id: Id) -> Option<Share> {
        times.get(id).map(|timed| timed.held.total())
    }

    #[test]
    fn a_profile_naming_no_action_or_none_of_the_graph_times_no_node() {
        let mut built = Built::default();
        let (_, _, bin) = built.target("app", "bin");
        built.action("bazel-out/bin/app/bin", bin);
        let run = timing(5, Runner::Local);

        assert_eq!(built.resolve(&Timings::default()), Err(Untimed::NoActions));
        assert_eq!(
            built.resolve(&timings(&[(None, None, run)])),
            Err(Untimed::Unnamed)
        );
        assert_eq!(
            built.resolve(&timings(&[
                (Some("bazel-out/bin/lib/lib.a"), None, run),
                (None, owner("//lib:lib"), run),
            ])),
            Err(Untimed::Unplaced)
        );
    }

    #[test]
    fn an_action_lands_on_its_node_and_its_time_on_every_container_above_it() {
        let mut built = Built::default();
        let (package, target, configured) = built.target("app", "bin");
        let compile = built.action("bazel-out/bin/app/bin.o", configured);
        let link = built.action("bazel-out/bin/app/bin", configured);
        let (_, _, idle) = built.target("app", "idle");
        let recorded = timings(&[
            (
                Some("bazel-out/bin/app/bin.o"),
                None,
                timing(30, Runner::RemoteExecution),
            ),
            (
                Some("bazel-out/bin/app/bin"),
                None,
                timing(12, Runner::Sandboxed),
            ),
        ]);

        let times = built.resolve(&recorded).expect("timed");

        assert_eq!(held(&times, compile), Some(share(1, 30)));
        assert_eq!(held(&times, link), Some(share(1, 12)));
        for container in [configured, target, package] {
            assert_eq!(held(&times, container), Some(share(2, 42)));
            assert_eq!(times.get(container).unwrap().own, Spent::default());
        }
        let spent = &times.get(package).unwrap().held;
        assert_eq!(spent.of(Runner::RemoteExecution), share(1, 30));
        assert_eq!(spent.of(Runner::Sandboxed), share(1, 12));
        assert_eq!(times.get(idle), None);
    }

    #[test]
    fn an_action_named_only_by_its_owner_lands_on_the_owner() {
        let mut built = Built::default();
        let (package, target, configured) = built.target("app", "bin");
        let other_package = built.node("pkg:lib", Kind::Package, None);
        let unconfigured = built.node("target://lib:lib", Kind::Target, Some(other_package));
        let recorded = timings(&[
            (None, owner("//app:bin"), timing(8, Runner::Sandboxed)),
            (None, owner("//app:bin"), timing(2, Runner::NoSpawnRecorded)),
            (None, owner("//lib:lib"), timing(5, Runner::Worker)),
            (None, owner("//gone:gone"), timing(100, Runner::Local)),
            (None, None, timing(1, Runner::Local)),
        ]);

        let times = built.resolve(&recorded).expect("timed");

        assert_eq!(
            times.get(configured).unwrap().own.total(),
            share(2, 10),
            "lands on the configured target it names"
        );
        for container in [target, package] {
            assert_eq!(held(&times, container), Some(share(2, 10)));
        }
        assert_eq!(held(&times, unconfigured), Some(share(1, 5)));
        assert_eq!(held(&times, other_package), Some(share(1, 5)));
    }

    #[test]
    fn an_action_the_graph_holds_no_node_for_lands_on_its_owner() {
        let mut built = Built::default();
        let (package, _, configured) = built.target("app", "bin");
        let recorded = timings(&[
            (
                Some("bazel-out/bin/app/bin"),
                owner("//app:bin"),
                timing(9, Runner::Sandboxed),
            ),
            (
                Some("bazel-out/bin/gone/gone"),
                owner("//gone:gone"),
                timing(1, Runner::Local),
            ),
        ]);

        let times = built.resolve(&recorded).expect("timed");

        assert_eq!(times.get(configured).unwrap().own.total(), share(1, 9));
        assert_eq!(held(&times, package), Some(share(1, 9)));
    }

    #[test]
    fn coverage_counts_the_profiled_actions_landing_on_a_node() {
        let mut built = Built::default();
        let (_, _, bin) = built.target("app", "bin");
        built.action("bazel-out/bin/app/bin", bin);
        let run = timing(1, Runner::Local);
        let recorded = timings(&[
            (Some("bazel-out/bin/app/bin"), None, run),
            (Some("bazel-out/bin/app/gen"), owner("//app:bin"), run),
            (Some("bazel-out/bin/gone/gone"), owner("//gone:gone"), run),
            (None, owner("//app:bin"), run),
            (None, owner("//app:bin"), run),
            (None, None, run),
        ]);

        let times = built.resolve(&recorded).expect("timed");

        assert_eq!(
            times.coverage(),
            Coverage {
                placed: 4,
                profiled: 6
            }
        );
    }

    #[test]
    fn each_kind_spans_the_least_to_the_most_time_its_nodes_hold() {
        let mut built = Built::default();
        let (app, _, bin) = built.target("app", "bin");
        let fast = built.action("bazel-out/fast", bin);
        let slow = built.action("bazel-out/slow", bin);
        let (lib_package, _, lib) = built.target("lib", "lib");
        let middling = built.action("bazel-out/middling", lib);
        let recorded = timings(&[
            (Some("bazel-out/fast"), None, timing(10, Runner::Local)),
            (Some("bazel-out/slow"), None, timing(1_000, Runner::Local)),
            (Some("bazel-out/middling"), None, timing(100, Runner::Local)),
        ]);

        let times = built.resolve(&recorded).expect("timed");
        let place = |kind, id| {
            let extent = times.extent(kind).expect("timed kind");
            extent.place(times.get(id).unwrap().held.total().wall)
        };

        assert_eq!(place(Kind::Action, fast), 0.0);
        assert_eq!(place(Kind::Action, slow), 1.0);
        assert!((place(Kind::Action, middling) - 0.5).abs() < 1e-6);
        assert_eq!(place(Kind::ConfiguredTarget, bin), 1.0);
        assert_eq!(place(Kind::ConfiguredTarget, lib), 0.0);
        assert_eq!(place(Kind::Package, app), 1.0);
        assert_eq!(place(Kind::Package, lib_package), 0.0);
        assert_eq!(times.extent(Kind::Source), None);
        let once = Duration::from_millis(3);
        assert_eq!(
            Extent::of(once).place(once),
            1.0,
            "one time places at the most"
        );
        let mut instant = Extent::of(Duration::ZERO);
        instant.widen(Duration::from_secs(1));
        assert!(
            (instant.place(Duration::from_millis(31)) - 0.5).abs() < 0.01,
            "a time of none is placed as the shortest placed"
        );
    }

    /// The ids are handed out so that one container precedes what it holds
    /// and another follows it, and a target holds time of its own beside an
    /// action of its own.
    #[test]
    fn a_container_holds_its_own_time_and_all_it_contains_whatever_the_id_order() {
        let mut built = Built::default();
        let action = built.node("action:bazel-out/t.log", Kind::Action, None);
        let (package, target, configured) = built.target("tests", "check");
        built.graph.nodes[action.0 as usize].parent = Some(configured);
        let recorded = timings(&[
            (Some("bazel-out/t.log"), None, timing(20, Runner::Local)),
            (
                None,
                owner("//tests:check"),
                timing(4, Runner::DiskCacheHit),
            ),
        ]);

        let times = built.resolve(&recorded).expect("timed");

        assert_eq!(held(&times, action), Some(share(1, 20)));
        assert_eq!(times.get(configured).unwrap().own.total(), share(1, 4));
        for container in [configured, target, package] {
            assert_eq!(held(&times, container), Some(share(2, 24)));
        }
        let group = Id(built.names.len() as u32);
        assert_eq!(times.get(group), None);
    }
}
