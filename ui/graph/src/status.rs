//! What a build reports of the nodes of its graph.

use bviz_graph_layout::{Graph, Id, Kind, Names, Resolver};
use bviz_invocation::{BuildOutcome, Cause, ConfiguredTarget, Outcomes, TestStatus};
use rustc_hash::FxHashMap;
use std::cell::RefCell;

/// How the build ended for a node, from the least severe to the most. Of
/// several a node comes to — its build and its test, two configurations
/// drawn as one target, the nodes a container holds — it takes the most
/// severe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Status {
    /// Left out of the build as incompatible with the target platform. A
    /// build that skips a target ends no worse for it, so this ranks lowest
    /// and shows only where nothing else is reported.
    Skipped,
    /// Built, and passed where it is a test that ran.
    Succeeded,
    /// The build ended before the node was built, or before its test had
    /// run to an end.
    Unfinished,
    /// A test that passed on a later attempt than its first.
    Flaky,
    /// A test that was built, ran and did not pass.
    TestFailed,
    /// Not built because something it depends on failed.
    Failed,
    /// What a failure traces back to: an action that failed, a target that
    /// could not be analysed, or a failed target the build traces to nothing
    /// the graph holds.
    RootCause,
}

impl Status {
    /// Every status, in the order of the enum.
    pub(crate) const ALL: [Status; 7] = [
        Status::Skipped,
        Status::Succeeded,
        Status::Unfinished,
        Status::Flaky,
        Status::TestFailed,
        Status::Failed,
        Status::RootCause,
    ];

    /// The name a reader is shown the status by.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Status::Skipped => "skipped",
            Status::Succeeded => "succeeded",
            Status::Unfinished => "not finished",
            Status::Flaky => "flaky",
            Status::TestFailed => "test failed",
            Status::Failed => "dependency failed",
            Status::RootCause => "root cause",
        }
    }

    /// The status of a target whose build ended as `outcome`. One that failed
    /// is `traced` where the graph holds a node its failure traces back to,
    /// and is the root cause itself where it holds none.
    pub(crate) fn of_build(outcome: &BuildOutcome, traced: bool) -> Status {
        match outcome {
            BuildOutcome::Built => Status::Succeeded,
            BuildOutcome::Skipped => Status::Skipped,
            BuildOutcome::NotBuilt => Status::Unfinished,
            BuildOutcome::Failed { .. } | BuildOutcome::AnalysisFailed { .. } => match traced {
                true => Status::Failed,
                false => Status::RootCause,
            },
        }
    }

    /// A test the build could not build failed through something it depends
    /// on, which its build outcome traces back.
    pub(crate) fn of_test(status: TestStatus) -> Status {
        match status {
            TestStatus::Passed => Status::Succeeded,
            TestStatus::Flaky => Status::Flaky,
            TestStatus::Failed | TestStatus::Timeout | TestStatus::RemoteFailure => {
                Status::TestFailed
            }
            TestStatus::NoStatus | TestStatus::Incomplete | TestStatus::ToolHaltedBeforeTesting => {
                Status::Unfinished
            }
            TestStatus::FailedToBuild => Status::Failed,
        }
    }
}

/// The [`Status`] of every node of one graph a build reports on. A build
/// reports on the targets it was asked for, so most nodes of a graph have
/// none, and both tables are empty where it reports on no node of the graph.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Statuses {
    /// By node id, the most severe status the build reports of the node
    /// itself.
    own: Vec<Option<Status>>,
    /// By node id, the most severe status of the node and of every node it
    /// contains.
    worst: Vec<Option<Status>>,
}

impl Statuses {
    /// What the outcomes `nodes` lands report of the nodes of `graph`.
    pub(crate) fn resolve<'a>(nodes: &Nodes<'a>, graph: &Graph) -> Self {
        let outcomes = nodes.outcomes;
        let mut reported = Reported {
            nodes,
            own: vec![None; nodes.landings.ids()],
        };
        for (primary_output, _) in outcomes.failed_actions() {
            if let Some(action) = nodes.of_action(primary_output) {
                reported.report(action, Status::RootCause);
            }
        }
        for (target, outcome) in outcomes.builds() {
            let status = reported.build_status(outcome);
            if let Some(node) = nodes.of_target(target) {
                reported.report(node, status);
            }
        }
        for (target, test) in outcomes.tests() {
            if let Some(node) = nodes.of_target(target) {
                reported.report(node, Status::of_test(test.status));
            }
        }
        if reported.own.iter().all(Option::is_none) {
            return Self::default();
        }
        Self {
            worst: rolled_up(reported.own.clone(), graph),
            own: reported.own,
        }
    }

    /// The status the build reports of the node `id` names itself, whatever
    /// it holds.
    pub(crate) fn own(&self, id: Id) -> Option<Status> {
        self.own.get(id.0 as usize).copied().flatten()
    }

    /// The status the node `id` names stands at, for itself and for what it
    /// holds. An id past the graph's own, as a merge group's is, reads as a
    /// node the build reports nothing of.
    pub(crate) fn get(&self, id: Id) -> Option<Status> {
        self.worst.get(id.0 as usize).copied().flatten()
    }
}

/// Where a report on a configured target lands in a graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Landing {
    /// On the configured target, or on the target where the report names no
    /// configuration or the graph holds the target under none.
    On(Id),
    /// On the target, which the graph holds under configurations that are
    /// all other than the one reported.
    Reconfigured(Id),
    Off,
}

impl Landing {
    pub(crate) fn node(self) -> Option<Id> {
        match self {
            Landing::On(node) | Landing::Reconfigured(node) => Some(node),
            Landing::Off => None,
        }
    }
}

/// Where reports naming configured targets land in one graph. Each target is
/// found once, however many readers ask for it.
pub(crate) struct Landings<'a> {
    resolver: Resolver<'a>,
    /// By node id, whether the node holds a configured target.
    configured: Vec<bool>,
    targets: RefCell<FxHashMap<ConfiguredTarget<&'a str>, Landing>>,
}

impl<'a> Landings<'a> {
    /// The landings in `graph`, which `names` names, sized for reports naming
    /// `targets` configured targets.
    pub(crate) fn new(graph: &Graph, names: &'a Names, targets: usize) -> Self {
        let mut configured = vec![false; names.len()];
        for node in &graph.nodes {
            if let (Kind::ConfiguredTarget, Some(target)) = (node.kind, node.parent) {
                configured[target.0 as usize] = true;
            }
        }
        Self {
            resolver: Resolver::new(names),
            configured,
            targets: RefCell::new(FxHashMap::with_capacity_and_hasher(
                targets,
                Default::default(),
            )),
        }
    }

    pub(crate) fn landing(&self, target: ConfiguredTarget<&'a str>) -> Landing {
        if let Some(landing) = self.targets.borrow().get(&target) {
            return *landing;
        }
        let label = target.label();
        let configured = target
            .configuration()
            .and_then(|checksum| self.resolver.configured_target(label, checksum));
        let landing = match (configured, self.resolver.target(label)) {
            (Some(configured), _) => Landing::On(configured),
            (None, Some(node)) => {
                let elsewhere =
                    target.configuration().is_some() && self.configured[node.0 as usize];
                match elsewhere {
                    true => Landing::Reconfigured(node),
                    false => Landing::On(node),
                }
            }
            (None, None) => Landing::Off,
        };
        self.targets.borrow_mut().insert(target, landing);
        landing
    }

    /// The node a report on the action writing `primary_output` lands on: the
    /// action, or where the graph holds no node for it, its `owner`.
    pub(crate) fn action(
        &self,
        primary_output: &str,
        owner: Option<ConfiguredTarget<&'a str>>,
    ) -> Option<Id> {
        self.resolver
            .action(primary_output)
            .or_else(|| self.landing(owner?).node())
    }

    /// The number of node ids of the graph, past which an id is none of its
    /// nodes'.
    pub(crate) fn ids(&self) -> usize {
        self.configured.len()
    }
}

/// The nodes of a graph a build's outcomes name. Each is found once, however
/// many readers ask for it.
pub(crate) struct Nodes<'a> {
    landings: Landings<'a>,
    outcomes: &'a Outcomes,
    /// By primary output, the node the failure of the action writing it
    /// lands on.
    actions: RefCell<FxHashMap<&'a str, Option<Id>>>,
}

impl<'a> Nodes<'a> {
    /// The nodes `outcomes` names among those of `graph`, which `names`
    /// names.
    pub(crate) fn new(graph: &Graph, names: &'a Names, outcomes: &'a Outcomes) -> Self {
        let targets = outcomes.builds().count() + outcomes.tests().count();
        let actions = outcomes.failed_actions().count();
        Self {
            landings: Landings::new(graph, names, targets),
            outcomes,
            actions: RefCell::new(FxHashMap::with_capacity_and_hasher(
                actions,
                Default::default(),
            )),
        }
    }

    pub(crate) fn outcomes(&self) -> &'a Outcomes {
        self.outcomes
    }

    pub(crate) fn landing(&self, target: ConfiguredTarget<&'a str>) -> Landing {
        self.landings.landing(target)
    }

    pub(crate) fn of_target(&self, target: ConfiguredTarget<&'a str>) -> Option<Id> {
        self.landing(target).node()
    }

    /// The node the failure of the action writing `primary_output` lands on.
    pub(crate) fn of_action(&self, primary_output: &'a str) -> Option<Id> {
        if let Some(node) = self.actions.borrow().get(primary_output) {
            return *node;
        }
        let owner = self
            .outcomes
            .failed_action(primary_output)
            .and_then(|failed| failed.owner.as_ref())
            .map(ConfiguredTarget::borrowed);
        let node = self.landings.action(primary_output, owner);
        self.actions.borrow_mut().insert(primary_output, node);
        node
    }

    pub(crate) fn of_cause(&self, cause: &'a Cause) -> Option<Id> {
        match cause {
            Cause::Action { primary_output } => self.of_action(primary_output),
            Cause::Target(target) => self.of_target(target.borrowed()),
        }
    }
}

/// What a build reports of each node itself, by node id.
struct Reported<'n, 'a> {
    nodes: &'n Nodes<'a>,
    own: Vec<Option<Status>>,
}

impl<'a> Reported<'_, 'a> {
    fn report(&mut self, node: Id, status: Status) {
        let own = &mut self.own[node.0 as usize];
        *own = (*own).max(Some(status));
    }

    /// The status `outcome` gives the target it is reported of, each node a
    /// failure traces back to being reported as that.
    fn build_status(&mut self, outcome: &'a BuildOutcome) -> Status {
        let mut traced = false;
        if let BuildOutcome::Failed { causes } | BuildOutcome::AnalysisFailed { causes } = outcome {
            for cause in causes {
                if let Some(node) = self.nodes.of_cause(cause) {
                    self.report(node, Status::RootCause);
                    traced = true;
                }
            }
        }
        Status::of_build(outcome, traced)
    }
}

/// `own` with each node of `graph` raised to the most severe status among
/// the nodes it contains.
fn rolled_up(own: Vec<Option<Status>>, graph: &Graph) -> Vec<Option<Status>> {
    let mut parent = vec![None; own.len()];
    for node in &graph.nodes {
        parent[node.id.0 as usize] = node.parent;
    }
    let mut worst = own;
    for node in 0..worst.len() {
        let status = worst[node];
        if status.is_none() {
            continue;
        }
        // A walk stops at the first container already as severe, which either
        // was raised by a walk that went on above it or raises those above it
        // in its own turn.
        let mut at = parent[node];
        while let Some(container) = at.filter(|c| worst[c.0 as usize] < status) {
            worst[container.0 as usize] = status;
            at = parent[container.0 as usize];
        }
    }
    worst
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_graph_layout::Kind;
    use bviz_graph_layout::graph::Node;
    use bviz_invocation::{FailedAction, TestOutcome};
    use std::num::NonZeroI32;
    use std::time::Duration;

    const CONFIG: &str = "a7a71f";
    const OTHER_CONFIG: &str = "960ec4";
    const GEN_OUTPUT: &str = "bazel-out/k8/bin/gen/gen.out";

    /// A graph built node by node, each under the id its name interns to.
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

        fn resolve(&self, outcomes: &Outcomes) -> Statuses {
            let nodes = Nodes::new(&self.graph, &self.names, outcomes);
            Statuses::resolve(&nodes, &self.graph)
        }
    }

    fn target(label: &str, configuration: Option<&str>) -> ConfiguredTarget {
        ConfiguredTarget::new(label.into(), configuration.map(Into::into))
    }

    fn failed(causes: Vec<Cause>) -> BuildOutcome {
        BuildOutcome::Failed { causes }
    }

    fn gen_output() -> Cause {
        Cause::Action {
            primary_output: GEN_OUTPUT.into(),
        }
    }

    fn failed_action(owner: Option<ConfiguredTarget>) -> FailedAction {
        FailedAction {
            owner,
            mnemonic: "Genrule".into(),
            exit_code: NonZeroI32::new(1),
            message: "exit 1".into(),
        }
    }

    fn test(status: TestStatus) -> TestOutcome {
        TestOutcome {
            status,
            attempts: 1,
            runs: 1,
            shards: None,
            actions: 1,
            cached_actions: 0,
            run_duration: Duration::from_millis(319),
        }
    }

    #[test]
    fn a_build_reporting_on_no_node_gives_no_node_a_status() {
        let mut built = Built::default();
        let (package, _, configured) = built.target("app", "bin");

        let unreported = built.resolve(&Outcomes::default());
        assert_eq!(unreported, Statuses::default());
        assert_eq!(unreported.get(configured), None);

        let mut elsewhere = Outcomes::default();
        elsewhere.report_build(target("//lib:lib", Some(CONFIG)), BuildOutcome::Built);
        elsewhere.report_failed_action("bazel-out/k8/bin/lib/lib.o".into(), failed_action(None));
        let unrelated = built.resolve(&elsewhere);
        assert_eq!(unrelated.get(package), None);
        assert_eq!(unrelated.get(configured), None);
    }

    #[test]
    fn a_report_lands_on_a_target_reconfigured_only_where_the_graph_configures_it_otherwise() {
        let mut built = Built::default();
        let (_, bin, configured) = built.target("app", "bin");
        let package = built.node("pkg:lib", Kind::Package, None);
        let lib = built.node("target://lib:lib", Kind::Target, Some(package));
        let outcomes = Outcomes::default();
        let nodes = Nodes::new(&built.graph, &built.names, &outcomes);
        let landing =
            |label, configuration| nodes.landing(ConfiguredTarget::new(label, configuration));

        assert_eq!(landing("//app:bin", Some(CONFIG)), Landing::On(configured));
        assert_eq!(
            landing("//app:bin", Some(OTHER_CONFIG)),
            Landing::Reconfigured(bin)
        );
        assert_eq!(landing("//app:bin", None), Landing::On(bin));
        assert_eq!(landing("//lib:lib", Some(CONFIG)), Landing::On(lib));
        assert_eq!(landing("//lib:other", Some(CONFIG)), Landing::Off);
    }

    #[test]
    fn an_id_past_the_graphs_own_has_no_status() {
        let mut built = Built::default();
        let (_, _, bin) = built.target("app", "bin");
        let mut outcomes = Outcomes::default();
        outcomes.report_build(target("//app:bin", Some(CONFIG)), BuildOutcome::Built);

        let statuses = built.resolve(&outcomes);

        assert_eq!(statuses.get(bin), Some(Status::Succeeded));
        let group = Id(built.names.len() as u32);
        assert_eq!(statuses.get(group), None);
    }

    #[test]
    fn a_report_lands_on_the_configured_target_it_names() {
        let mut built = Built::default();
        let (_, _, bin) = built.target("app", "bin");
        let (_, _, lib) = built.target("app", "lib");
        let mut outcomes = Outcomes::default();
        outcomes.report_build(target("//app:bin", Some(CONFIG)), BuildOutcome::Built);

        let statuses = built.resolve(&outcomes);

        assert_eq!(statuses.get(bin), Some(Status::Succeeded));
        assert_eq!(statuses.get(lib), None);
    }

    #[test]
    fn a_report_the_graph_holds_no_configured_target_for_lands_on_the_target() {
        let mut built = Built::default();
        let package = built.node("pkg:app", Kind::Package, None);
        let bin = built.node("target://app:bin", Kind::Target, Some(package));
        let (_, lib, lib_configured) = built.target("app", "lib");
        let mut outcomes = Outcomes::default();
        outcomes.report_build(target("//app:bin", Some(CONFIG)), BuildOutcome::Skipped);
        outcomes.report_build(target("//app:lib", None), BuildOutcome::NotBuilt);
        outcomes.report_build(target("//app:lib", Some(OTHER_CONFIG)), BuildOutcome::Built);

        let statuses = built.resolve(&outcomes);

        assert_eq!(statuses.get(bin), Some(Status::Skipped));
        assert_eq!(statuses.get(lib), Some(Status::Unfinished));
        assert_eq!(statuses.get(lib_configured), None);
    }

    #[test]
    fn a_failed_action_is_the_root_cause_and_what_depends_on_it_failed() {
        let mut built = Built::default();
        let (gen_package, gen_target, gen_configured) = built.target("gen", "gen");
        let action = built.action(GEN_OUTPUT, gen_configured);
        let (app_package, app_target, app) = built.target("app", "bin");
        let mut outcomes = Outcomes::default();
        outcomes.report_failed_action(
            GEN_OUTPUT.into(),
            failed_action(Some(target("//gen:gen", Some(CONFIG)))),
        );
        for label in ["//gen:gen", "//app:bin"] {
            outcomes.report_build(target(label, Some(CONFIG)), failed(vec![gen_output()]));
        }

        let statuses = built.resolve(&outcomes);

        assert_eq!(statuses.get(action), Some(Status::RootCause));
        for holding in [gen_configured, gen_target, gen_package] {
            assert_eq!(statuses.get(holding), Some(Status::RootCause));
        }
        for dependent in [app, app_target, app_package] {
            assert_eq!(statuses.get(dependent), Some(Status::Failed));
        }
    }

    #[test]
    fn a_failed_action_the_graph_does_not_hold_lands_on_its_owner() {
        let mut built = Built::default();
        let (_, _, generator) = built.target("gen", "gen");
        let (_, _, app) = built.target("app", "bin");
        let mut outcomes = Outcomes::default();
        outcomes.report_failed_action(
            GEN_OUTPUT.into(),
            failed_action(Some(target("//gen:gen", Some(CONFIG)))),
        );
        outcomes.report_build(
            target("//app:bin", Some(CONFIG)),
            failed(vec![gen_output()]),
        );

        let statuses = built.resolve(&outcomes);

        assert_eq!(statuses.get(generator), Some(Status::RootCause));
        assert_eq!(statuses.get(app), Some(Status::Failed));
    }

    #[test]
    fn a_failed_target_traced_to_nothing_the_graph_holds_is_its_own_root_cause() {
        let mut built = Built::default();
        let (_, _, uncaused) = built.target("app", "bin");
        let (_, _, untraced) = built.target("app", "lib");
        let mut outcomes = Outcomes::default();
        outcomes.report_build(target("//app:bin", Some(CONFIG)), failed(Vec::new()));
        outcomes.report_build(
            target("//app:lib", Some(CONFIG)),
            failed(vec![gen_output()]),
        );

        let statuses = built.resolve(&outcomes);

        assert_eq!(statuses.get(uncaused), Some(Status::RootCause));
        assert_eq!(statuses.get(untraced), Some(Status::RootCause));
    }

    #[test]
    fn the_target_an_analysis_failure_names_is_its_root_cause() {
        let mut built = Built::default();
        let (_, app, app_configured) = built.target("app", "bin");
        let package = built.node("pkg:lib", Kind::Package, None);
        let broken = built.node("target://lib:broken", Kind::Target, Some(package));
        let mut outcomes = Outcomes::default();
        outcomes.report_build(
            target("//app:bin", None),
            BuildOutcome::AnalysisFailed {
                causes: vec![Cause::Target(target("//lib:broken", Some(CONFIG)))],
            },
        );

        let statuses = built.resolve(&outcomes);

        assert_eq!(statuses.get(broken), Some(Status::RootCause));
        assert_eq!(statuses.get(app), Some(Status::Failed));
        assert_eq!(statuses.get(app_configured), None);
    }

    #[test]
    fn a_test_takes_the_more_severe_of_its_build_and_its_run() {
        let ran = [
            (TestStatus::Passed, Status::Succeeded),
            (TestStatus::Flaky, Status::Flaky),
            (TestStatus::Failed, Status::TestFailed),
            (TestStatus::Timeout, Status::TestFailed),
            (TestStatus::RemoteFailure, Status::TestFailed),
            (TestStatus::NoStatus, Status::Unfinished),
            (TestStatus::Incomplete, Status::Unfinished),
            (TestStatus::ToolHaltedBeforeTesting, Status::Unfinished),
        ];
        for (run, expected) in ran {
            let mut built = Built::default();
            let (_, _, check) = built.target("app", "check");
            let mut outcomes = Outcomes::default();
            outcomes.report_build(target("//app:check", Some(CONFIG)), BuildOutcome::Built);
            outcomes.report_test(target("//app:check", Some(CONFIG)), test(run));

            let statuses = built.resolve(&outcomes);

            assert_eq!(statuses.get(check), Some(expected), "{run:?}");
        }
    }

    #[test]
    fn a_test_that_could_not_be_built_failed_whatever_its_build_outcome() {
        let mut built = Built::default();
        let (_, _, check) = built.target("app", "check");
        let (_, _, aliased) = built.target("app", "aliased");
        let (_, _, generator) = built.target("gen", "gen");
        let mut outcomes = Outcomes::default();
        outcomes.report_failed_action(
            GEN_OUTPUT.into(),
            failed_action(Some(target("//gen:gen", Some(CONFIG)))),
        );
        outcomes.report_build(
            target("//app:check", Some(CONFIG)),
            failed(vec![gen_output()]),
        );
        outcomes.report_test(
            target("//app:check", Some(CONFIG)),
            test(TestStatus::FailedToBuild),
        );
        // A test asked for through an alias has no build outcome under its
        // own label.
        outcomes.report_test(
            target("//app:aliased", Some(CONFIG)),
            test(TestStatus::FailedToBuild),
        );

        let statuses = built.resolve(&outcomes);

        assert_eq!(statuses.get(check), Some(Status::Failed));
        assert_eq!(statuses.get(aliased), Some(Status::Failed));
        assert_eq!(statuses.get(generator), Some(Status::RootCause));
    }

    #[test]
    fn a_skipped_target_shows_only_where_nothing_else_is_reported() {
        let mut built = Built::default();
        let (app_package, _, bin) = built.target("app", "bin");
        let (_, windows_target, windows) = built.target("app", "windows_only");
        let (tools_package, _, _) = built.target("tools", "windows_only");
        let mut outcomes = Outcomes::default();
        outcomes.report_build(target("//app:bin", Some(CONFIG)), BuildOutcome::Built);
        for label in ["//app:windows_only", "//tools:windows_only"] {
            outcomes.report_build(target(label, Some(CONFIG)), BuildOutcome::Skipped);
        }

        let statuses = built.resolve(&outcomes);

        assert_eq!(statuses.get(bin), Some(Status::Succeeded));
        assert_eq!(statuses.get(windows), Some(Status::Skipped));
        assert_eq!(statuses.get(windows_target), Some(Status::Skipped));
        assert_eq!(statuses.get(app_package), Some(Status::Succeeded));
        assert_eq!(statuses.get(tools_package), Some(Status::Skipped));
    }

    #[test]
    fn all_holds_every_status_in_the_order_of_the_enum() {
        // Exhaustive, so that a status the enum gains has to be placed here.
        let next = |status| match status {
            Status::Skipped => Some(Status::Succeeded),
            Status::Succeeded => Some(Status::Unfinished),
            Status::Unfinished => Some(Status::Flaky),
            Status::Flaky => Some(Status::TestFailed),
            Status::TestFailed => Some(Status::Failed),
            Status::Failed => Some(Status::RootCause),
            Status::RootCause => None,
        };
        let mut walked = Vec::with_capacity(Status::ALL.len());
        let mut at = Some(Status::Skipped);
        while let Some(status) = at {
            assert_eq!(status as usize, walked.len(), "{status:?}");
            walked.push(status);
            at = next(status);
        }
        assert_eq!(walked, Status::ALL);
    }

    #[test]
    fn several_reports_on_one_node_come_to_the_most_severe() {
        let mut built = Built::default();
        let package = built.node("pkg:app", Kind::Package, None);
        let bin = built.node("target://app:bin", Kind::Target, Some(package));
        let mut outcomes = Outcomes::default();
        outcomes.report_build(target("//app:bin", Some(CONFIG)), BuildOutcome::Built);
        outcomes.report_build(
            target("//app:bin", Some(OTHER_CONFIG)),
            BuildOutcome::NotBuilt,
        );

        let statuses = built.resolve(&outcomes);

        assert_eq!(statuses.get(bin), Some(Status::Unfinished));
    }

    #[test]
    fn a_container_takes_the_most_severe_status_it_holds() {
        // The ids are handed out so that one container precedes what it holds
        // and another follows it.
        let mut built = Built::default();
        let (app_package, bin_target, bin) = built.target("app", "bin");
        let (_, lib_target, lib) = built.target("app", "lib");
        let check = built.node("ct://tests:check#a7a71f", Kind::ConfiguredTarget, None);
        let tests = built.node("pkg:tests", Kind::Package, None);
        built.graph.nodes[check.0 as usize].parent = Some(tests);
        let idle = built.node("pkg:idle", Kind::Package, None);
        let mut outcomes = Outcomes::default();
        outcomes.report_build(target("//app:bin", Some(CONFIG)), BuildOutcome::Built);
        outcomes.report_build(target("//app:lib", Some(CONFIG)), BuildOutcome::NotBuilt);
        outcomes.report_build(target("//tests:check", Some(CONFIG)), BuildOutcome::Built);
        outcomes.report_test(
            target("//tests:check", Some(CONFIG)),
            test(TestStatus::Flaky),
        );

        let statuses = built.resolve(&outcomes);

        assert_eq!(statuses.get(bin), Some(Status::Succeeded));
        assert_eq!(statuses.get(bin_target), Some(Status::Succeeded));
        assert_eq!(statuses.get(lib), Some(Status::Unfinished));
        assert_eq!(statuses.get(lib_target), Some(Status::Unfinished));
        assert_eq!(statuses.get(app_package), Some(Status::Unfinished));
        assert_eq!(statuses.get(tests), Some(Status::Flaky));
        assert_eq!(statuses.get(idle), None);

        assert_eq!(statuses.own(lib), Some(Status::Unfinished));
        for holding in [bin_target, lib_target, app_package, tests, idle] {
            assert_eq!(statuses.own(holding), None);
        }
    }
}
