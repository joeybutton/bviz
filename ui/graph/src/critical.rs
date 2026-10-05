//! Where the steps of an invocation's critical path land on its graph.

use crate::status::Landings;
use bviz_graph_layout::{Graph, Id, Names};
use bviz_invocation::CriticalStep;

/// How a node stands to the critical path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum OnPath {
    #[default]
    Off,
    /// A step lands on a node the node contains.
    Holds,
    /// A step lands on the node itself.
    Own,
}

/// Where the steps of a critical path land on the nodes of one graph: on the
/// node of a step's action, or where the graph holds none, on the target
/// owning it.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct CriticalNodes {
    /// By step, in the order the steps started, the node it lands on.
    steps: Vec<Option<Id>>,
    /// By node id.
    on_path: Vec<OnPath>,
}

impl CriticalNodes {
    /// Places `steps` on the nodes of `graph`, which `names` names.
    pub(crate) fn resolve(graph: &Graph, names: &Names, steps: &[CriticalStep]) -> Self {
        let landings = Landings::new(graph, names, steps.len());
        let landed: Vec<Option<Id>> = steps
            .iter()
            .map(|step| match step.primary_output() {
                Some(primary_output) => landings.action(primary_output, step.owner()),
                None => landings.landing(step.owner()?).node(),
            })
            .collect();
        if landed.iter().all(Option::is_none) {
            return Self::default();
        }
        let mut parent = vec![None; landings.ids()];
        for node in &graph.nodes {
            parent[node.id.0 as usize] = node.parent;
        }
        let mut on_path = vec![OnPath::Off; landings.ids()];
        for &node in landed.iter().flatten() {
            on_path[node.0 as usize] = OnPath::Own;
            let mut at = parent[node.0 as usize];
            // A walk stops at a container another walk already reached, which
            // marked those above it in its turn.
            while let Some(container) = at.filter(|c| on_path[c.0 as usize] == OnPath::Off) {
                on_path[container.0 as usize] = OnPath::Holds;
                at = parent[container.0 as usize];
            }
        }
        Self {
            steps: landed,
            on_path,
        }
    }

    /// Whether any step lands on the graph.
    pub(crate) fn any(&self) -> bool {
        !self.steps.is_empty()
    }

    /// The node step `index` lands on, if it lands on the graph.
    pub(crate) fn step(&self, index: usize) -> Option<Id> {
        self.steps.get(index).copied().flatten()
    }

    /// Whether the box drawing the node `id` stands for a step: the step's
    /// own node, or, drawn `closed` over what it contains, one holding a
    /// step's node. An open box leaves the mark to the boxes it draws inside
    /// it. An id past the graph's own, as a merge group's is, stands for none.
    pub(crate) fn marks(&self, id: Id, closed: bool) -> bool {
        match self.on_path.get(id.0 as usize) {
            Some(OnPath::Own) => true,
            Some(OnPath::Holds) => closed,
            Some(OnPath::Off) | None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_graph_layout::Kind;
    use bviz_graph_layout::graph::Node;
    use bviz_invocation::ConfiguredTarget;

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

        /// The package `//{package}` holding the target `name`, configured
        /// under [`CONFIG`]: the target and the configured target.
        fn target(&mut self, package: &str, name: &str) -> (Id, Id) {
            let label = format!("//{package}:{name}");
            let package = self.node(&format!("pkg:{package}"), Kind::Package, None);
            let target = self.node(&format!("target:{label}"), Kind::Target, Some(package));
            let configured = self.node(
                &format!("ct:{label}#{CONFIG}"),
                Kind::ConfiguredTarget,
                Some(target),
            );
            (target, configured)
        }

        fn action(&mut self, primary_output: &str, owner: Id) -> Id {
            self.node(
                &format!("action:{primary_output}"),
                Kind::Action,
                Some(owner),
            )
        }

        fn resolve(&self, steps: &[CriticalStep]) -> CriticalNodes {
            CriticalNodes::resolve(&self.graph, &self.names, steps)
        }
    }

    fn step(primary_output: Option<&str>, owner: &str) -> CriticalStep {
        CriticalStep::new(
            primary_output,
            Some(ConfiguredTarget::new(owner, Some(CONFIG))),
        )
    }

    #[test]
    fn a_step_lands_on_its_action_and_else_on_the_target_owning_it() {
        let mut built = Built::default();
        let (_, compiled) = built.target("a", "lib");
        let compile = built.action("bazel-out/a/lib.o", compiled);
        let (_, linked) = built.target("b", "bin");
        let (_, tested) = built.target("t", "test");
        let steps = [
            step(Some("bazel-out/a/lib.o"), "//a:lib"),
            step(Some("bazel-out/b/bin"), "//b:bin"),
            step(None, "//c:gone"),
            step(None, "//t:test"),
        ];
        let nodes = built.resolve(&steps);
        assert!(nodes.any());
        assert!(nodes.marks(compile, false));
        assert!(nodes.marks(linked, false), "owns an action the graph lacks");
        assert!(!nodes.marks(compiled, false), "open over the step's action");
        assert!(nodes.marks(tested, false), "names no primary output");
        assert_eq!(nodes.step(0), Some(compile));
        assert_eq!(nodes.step(1), Some(linked));
        assert_eq!(nodes.step(2), None);
        assert_eq!(nodes.step(3), Some(tested));
        assert_eq!(nodes.step(4), None);
    }

    #[test]
    fn a_closed_box_holding_a_step_stands_for_it_and_an_open_one_does_not() {
        let mut built = Built::default();
        let (target, configured) = built.target("a", "lib");
        let compile = built.action("bazel-out/a/lib.o", configured);
        let other = built.action("bazel-out/a/other.o", configured);
        let nodes = built.resolve(&[step(Some("bazel-out/a/lib.o"), "//a:lib")]);
        assert!(nodes.marks(compile, true) && nodes.marks(compile, false));
        assert!(!nodes.marks(other, true));
        for container in [target, configured] {
            assert!(nodes.marks(container, true));
            assert!(!nodes.marks(container, false));
        }
        assert!(!nodes.marks(Id(u32::MAX), true));
    }

    #[test]
    fn a_path_landing_nowhere_marks_nothing() {
        let mut built = Built::default();
        let (target, _) = built.target("a", "lib");
        let nodes = built.resolve(&[step(Some("bazel-out/x.o"), "//x:x")]);
        assert!(!nodes.any());
        assert!(!nodes.marks(target, true));
        assert_eq!(nodes, CriticalNodes::default());
    }
}
