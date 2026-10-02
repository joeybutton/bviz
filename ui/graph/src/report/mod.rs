//! What a build reported, as the card over the graph lists it.

use crate::status::{Landing, Nodes, Status};
use bviz_graph_layout::Id;
use bviz_invocation::{
    BuildOutcome, Cause, ConfiguredTarget, FailedAction, TestOutcome, TestStatus,
};
use rustc_hash::FxHashMap;
use std::num::NonZeroI32;
use std::ops::Range;

mod card;
mod words;

pub(crate) use card::{Card, card, control};

/// How building ended for the targets a build reports on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct BuildTally {
    built: usize,
    failed: usize,
    not_built: usize,
    skipped: usize,
}

impl BuildTally {
    fn count(&mut self, outcome: &BuildOutcome) {
        let counted = match outcome {
            BuildOutcome::Built => &mut self.built,
            BuildOutcome::Failed { .. } | BuildOutcome::AnalysisFailed { .. } => &mut self.failed,
            BuildOutcome::NotBuilt => &mut self.not_built,
            BuildOutcome::Skipped => &mut self.skipped,
        };
        *counted += 1;
    }

    fn total(&self) -> usize {
        self.built + self.failed + self.not_built + self.skipped
    }
}

/// Of the things of one kind a build reports on, how many the graph does not
/// hold.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Share {
    reported: usize,
    off: usize,
}

impl Share {
    fn count(&mut self, held: bool) {
        self.reported += 1;
        self.off += usize::from(!held);
    }
}

/// How far the graph holds what the build reports on. Of the ways it does
/// not, only a target held under other configurations says the graph was
/// read from dumps configured apart from the build.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Mismatches {
    targets: Share,
    /// The targets reported on that the graph holds under configurations
    /// that are all other than the one reported.
    reconfigured: usize,
    failed_actions: Share,
}

impl Mismatches {
    fn of(nodes: &Nodes) -> Self {
        let outcomes = nodes.outcomes();
        let mut mismatches = Mismatches::default();
        for (target, _) in outcomes.builds() {
            let landing = nodes.landing(target);
            mismatches.targets.count(landing != Landing::Off);
            mismatches.reconfigured += usize::from(matches!(landing, Landing::Reconfigured(_)));
        }
        for (primary_output, _) in outcomes.failed_actions() {
            let held = nodes.of_action(primary_output).is_some();
            mismatches.failed_actions.count(held);
        }
        mismatches
    }
}

/// How a target that is a root cause came to be one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unbuilt {
    /// The build reports that it could not be analysed.
    Unanalysed,
    /// The build reports that it failed, and names no cause.
    Uncaused,
    /// The build names it as a cause and reports no failure of it.
    Unreported,
}

/// A failed action, apart from what the build said of it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Action {
    /// The exec path of the primary output the action writes.
    primary_output: String,
    /// The target the action belongs to, where the build names it.
    owner: Option<ConfiguredTarget>,
    /// Empty where the build states none.
    mnemonic: String,
    /// The code the action's process exited with, where the build states
    /// one.
    exit_code: Option<NonZeroI32>,
}

impl Action {
    /// The action writing `primary_output`, as the build describes it:
    /// `described` is `None` for an action the build blames and says no more
    /// of.
    fn of(primary_output: &str, described: Option<&FailedAction>) -> Self {
        Action {
            primary_output: primary_output.to_owned(),
            owner: described.and_then(|action| action.owner.clone()),
            mnemonic: described.map_or_else(String::new, |action| action.mnemonic.clone()),
            exit_code: described.and_then(|action| action.exit_code),
        }
    }

    fn name(&self) -> Name<'_> {
        match &self.owner {
            Some(owner) => Name::Owned {
                mnemonic: &self.mnemonic,
                owner: owner.borrowed().label(),
            },
            None => Name::Writing {
                mnemonic: &self.mnemonic,
                primary_output: &self.primary_output,
            },
        }
    }
}

/// What a row of the card lists.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Listed {
    /// A failed action, the root cause of `failing` targets' failure.
    Action {
        action: Action,
        /// What the build said of the failure, empty where it said nothing.
        message: String,
        failing: usize,
    },
    /// A target that is the root cause of `failing` targets' failure.
    Target {
        target: ConfiguredTarget,
        came: Unbuilt,
        /// What the build said of the target, empty where it said nothing.
        error: String,
        failing: usize,
    },
    /// A test that did not pass on its first attempt.
    Test {
        target: ConfiguredTarget,
        test: TestOutcome,
    },
}

/// What a row names the thing it lists by, its configuration apart.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Name<'a> {
    /// An action, by its mnemonic and the label of the target owning it.
    Owned {
        mnemonic: &'a str,
        owner: &'a str,
    },
    /// An action the build names no owner of, by its mnemonic and the exec
    /// path of the primary output it writes.
    Writing {
        mnemonic: &'a str,
        primary_output: &'a str,
    },
    Label(&'a str),
}

impl Listed {
    fn status(&self) -> Status {
        match self {
            Listed::Action { .. } | Listed::Target { .. } => Status::RootCause,
            Listed::Test { test, .. } => Status::of_test(test.status),
        }
    }

    fn name(&self) -> Name<'_> {
        match self {
            Listed::Action { action, .. } => action.name(),
            Listed::Target { target, .. } | Listed::Test { target, .. } => {
                Name::Label(target.borrowed().label())
            }
        }
    }

    /// The configuration of the thing listed, which its [`Name`] leaves out.
    fn configuration(&self) -> Option<&str> {
        let configured = match self {
            Listed::Action { action, .. } => action.owner.as_ref()?,
            Listed::Target { target, .. } | Listed::Test { target, .. } => target,
        };
        configured.borrowed().configuration()
    }
}

/// The node a click on a row shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shown {
    /// The node of the thing listed.
    Itself(Id),
    /// The first target that failed through the thing listed, which the
    /// graph does not hold. Such a target stands as the root cause itself.
    Failed(Id),
    /// None: the graph holds neither.
    Nothing,
}

impl Shown {
    /// The node of the thing listed where the graph holds it at `itself`,
    /// and what `failed` finds where it does not.
    fn of(itself: Option<Id>, failed: impl FnOnce() -> Option<Id>) -> Self {
        match itself {
            Some(node) => Shown::Itself(node),
            None => failed().map_or(Shown::Nothing, Shown::Failed),
        }
    }

    fn node(self) -> Option<Id> {
        match self {
            Shown::Itself(node) | Shown::Failed(node) => Some(node),
            Shown::Nothing => None,
        }
    }
}

/// One thing the card lists.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    listed: Listed,
    /// Whether another row names its thing alike, so that the row names the
    /// configuration of its own to be told apart.
    alike: bool,
    shown: Shown,
}

/// Marks each row of `root_causes` and of `troubled` that another of either
/// names its thing alike.
fn tell_apart(root_causes: &mut [Row], troubled: &mut [Row]) {
    let rows = root_causes.len() + troubled.len();
    let mut named: FxHashMap<Name, usize> =
        FxHashMap::with_capacity_and_hasher(rows, Default::default());
    for row in root_causes.iter().chain(troubled.iter()) {
        *named.entry(row.listed.name()).or_default() += 1;
    }
    let alike: Vec<bool> = root_causes
        .iter()
        .chain(troubled.iter())
        .map(|row| named[&row.listed.name()] > 1)
        .collect();
    for (row, alike) in root_causes.iter_mut().chain(troubled).zip(alike) {
        row.alike = alike;
    }
}

/// What a failure traces back to.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Blamed<'a> {
    /// The action writing the primary output at this exec path.
    Action(&'a str),
    Target(ConfiguredTarget<&'a str>),
}

impl<'a> Blamed<'a> {
    fn of(cause: &'a Cause) -> Self {
        match cause {
            Cause::Action { primary_output } => Blamed::Action(primary_output),
            Cause::Target(target) => Blamed::Target(target.borrowed()),
        }
    }

    /// Orders the things blamed apart from how much each is blamed for.
    fn order(&self) -> (u8, &'a str, Option<&'a str>) {
        match self {
            Blamed::Action(primary_output) => (0, *primary_output, None),
            Blamed::Target(target) => (1, target.label(), target.configuration()),
        }
    }
}

/// What the build blames one thing for.
#[derive(Clone, Copy, Default)]
struct Blame<'a> {
    /// The targets reported as failed because of it.
    failing: usize,
    /// The first of those targets by label and configuration, so that it is
    /// the same one however the targets are walked.
    first: Option<ConfiguredTarget<&'a str>>,
}

impl<'a> Blame<'a> {
    fn fail(&mut self, target: ConfiguredTarget<&'a str>) {
        let order = |target: ConfiguredTarget<&'a str>| (target.label(), target.configuration());
        self.failing += 1;
        if self.first.is_none_or(|first| order(target) < order(first)) {
            self.first = Some(target);
        }
    }
}

/// The row of the root cause `blamed`, which the build blames for `blame`.
fn root_cause<'a>(blamed: Blamed<'a>, blame: Blame<'a>, nodes: &Nodes<'a>) -> Row {
    let outcomes = nodes.outcomes();
    let failing = blame.failing;
    let failed = || blame.first.and_then(|first| nodes.of_target(first));
    let (listed, node) = match blamed {
        Blamed::Action(primary_output) => {
            let described = outcomes.failed_action(primary_output);
            let listed = Listed::Action {
                action: Action::of(primary_output, described),
                message: described.map_or_else(String::new, |action| action.message.clone()),
                failing,
            };
            (listed, nodes.of_action(primary_output))
        }
        Blamed::Target(target) => {
            let came = match outcomes.build(target) {
                Some(BuildOutcome::AnalysisFailed { .. }) => Unbuilt::Unanalysed,
                Some(BuildOutcome::Failed { .. }) => Unbuilt::Uncaused,
                _ => Unbuilt::Unreported,
            };
            let listed = Listed::Target {
                target: target.owned(),
                came,
                error: outcomes.target_error(target).unwrap_or_default().to_owned(),
                failing,
            };
            (listed, nodes.of_target(target))
        }
    };
    Row {
        listed,
        alike: false,
        shown: Shown::of(node, failed),
    }
}

/// The root causes of the failures `nodes` lands, the one blamed for the most
/// targets first; `failed` is how many targets the build reports as failed.
fn root_causes(nodes: &Nodes, failed: usize) -> Vec<Row> {
    let outcomes = nodes.outcomes();
    let failed_actions = outcomes.failed_actions();
    let mut blame: FxHashMap<Blamed, Blame> = FxHashMap::with_capacity_and_hasher(
        failed + failed_actions.size_hint().0,
        Default::default(),
    );
    for (primary_output, _) in failed_actions {
        blame.entry(Blamed::Action(primary_output)).or_default();
    }
    for (target, outcome) in outcomes.builds() {
        let (BuildOutcome::Failed { causes } | BuildOutcome::AnalysisFailed { causes }) = outcome
        else {
            continue;
        };
        if causes.is_empty() {
            // A failure the build traces to nothing is its own root cause,
            // which fails no target but itself.
            blame.entry(Blamed::Target(target)).or_default();
        }
        for cause in causes {
            blame.entry(Blamed::of(cause)).or_default().fail(target);
        }
    }
    let mut blamed: Vec<(Blamed, Blame)> = blame.into_iter().collect();
    blamed.sort_by(|(one, one_blame), (other, other_blame)| {
        (other_blame.failing, one.order()).cmp(&(one_blame.failing, other.order()))
    });
    blamed
        .into_iter()
        .map(|(blamed, blame)| root_cause(blamed, blame, nodes))
        .collect()
}

/// Whether every result of `test` came from a cache.
fn wholly_cached(test: &TestOutcome) -> bool {
    test.actions > 0 && test.cached_actions == test.actions
}

/// How running ended for the tests a build reports on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TestTally {
    passed: usize,
    /// Those of `passed` that came wholly from a cache.
    cached: usize,
    flaky: usize,
    failed: usize,
    timed_out: usize,
    failed_remotely: usize,
    incomplete: usize,
    not_run: usize,
    not_built: usize,
}

impl TestTally {
    fn count(&mut self, test: &TestOutcome) {
        let counted = match test.status {
            TestStatus::Passed => {
                self.cached += usize::from(wholly_cached(test));
                &mut self.passed
            }
            TestStatus::Flaky => &mut self.flaky,
            TestStatus::Failed => &mut self.failed,
            TestStatus::Timeout => &mut self.timed_out,
            TestStatus::RemoteFailure => &mut self.failed_remotely,
            TestStatus::Incomplete => &mut self.incomplete,
            TestStatus::NoStatus | TestStatus::ToolHaltedBeforeTesting => &mut self.not_run,
            TestStatus::FailedToBuild => &mut self.not_built,
        };
        *counted += 1;
    }

    /// The tests that did not pass on their first attempt.
    fn troubled(&self) -> usize {
        self.total() - self.passed
    }

    fn total(&self) -> usize {
        self.passed
            + self.flaky
            + self.failed
            + self.timed_out
            + self.failed_remotely
            + self.incomplete
            + self.not_run
            + self.not_built
    }
}

/// Where a test stands among those listed, the worst first. `None` for a
/// test that passed, which is counted and not listed, and for one that could
/// not be built, whose failure the root causes account for.
fn listed_at(status: TestStatus) -> Option<u8> {
    match status {
        TestStatus::Failed => Some(0),
        TestStatus::Timeout => Some(1),
        TestStatus::RemoteFailure => Some(2),
        TestStatus::Flaky => Some(3),
        TestStatus::Incomplete => Some(4),
        TestStatus::NoStatus => Some(5),
        TestStatus::ToolHaltedBeforeTesting => Some(6),
        TestStatus::Passed | TestStatus::FailedToBuild => None,
    }
}

/// The rows of the tests `nodes` lands that did not pass on their first
/// attempt, of which there are `troubled`, the worst first.
fn troubled_tests(nodes: &Nodes, troubled: usize) -> Vec<Row> {
    let mut listed = Vec::with_capacity(troubled);
    listed.extend(
        nodes
            .outcomes()
            .tests()
            .filter_map(|(target, test)| Some((listed_at(test.status)?, target, test))),
    );
    listed.sort_by_key(|(at, target, _)| (*at, target.label(), target.configuration()));
    listed
        .into_iter()
        .map(|(_, target, test)| Row {
            listed: Listed::Test {
                target: target.owned(),
                test: test.clone(),
            },
            alike: false,
            shown: Shown::of(nodes.of_target(target), || None),
        })
        .collect()
}

/// What a build reported, as the card lists it.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Report {
    builds: BuildTally,
    tests: TestTally,
    mismatches: Mismatches,
    /// The root causes of the build's failures, the one blamed for the most
    /// targets first.
    root_causes: Vec<Row>,
    /// The tests that did not pass on their first attempt, the worst first.
    troubled: Vec<Row>,
}

impl Report {
    /// What the outcomes `nodes` lands report.
    pub(crate) fn of(nodes: &Nodes) -> Self {
        let mut builds = BuildTally::default();
        for (_, outcome) in nodes.outcomes().builds() {
            builds.count(outcome);
        }
        let mut tests = TestTally::default();
        for (_, test) in nodes.outcomes().tests() {
            tests.count(test);
        }
        let mut root_causes = root_causes(nodes, builds.failed);
        let mut troubled = troubled_tests(nodes, tests.troubled());
        tell_apart(&mut root_causes, &mut troubled);
        Self {
            builds,
            tests,
            mismatches: Mismatches::of(nodes),
            root_causes,
            troubled,
        }
    }

    /// How many rows the card lists.
    fn rows(&self) -> usize {
        self.root_causes.len() + self.troubled.len()
    }

    /// The rows the card lists at `range`, as many of them as there are: the
    /// root causes, and after them the tests.
    fn listed(&self, range: Range<usize>) -> impl Iterator<Item = &Row> {
        let rows = self.root_causes.iter().chain(&self.troubled);
        rows.skip(range.start).take(range.len())
    }

    /// Whether the build reported nothing the card could read.
    pub(crate) fn is_empty(&self) -> bool {
        self.builds.total() == 0 && self.tests.total() == 0 && self.rows() == 0
    }

    /// Whether the build left something for the reader to look into, which
    /// the card opens for unasked.
    fn troubled(&self) -> bool {
        self.builds.failed + self.builds.not_built + self.tests.troubled() > 0 || self.rows() > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_graph_layout::graph::Node;
    use bviz_graph_layout::{Graph, Kind, Names};
    use bviz_invocation::{Configurations, Outcomes};
    use std::num::{NonZeroI32, NonZeroU32};
    use std::time::Duration;

    const CONFIG: &str = "a7a71f";
    const EXEC_CONFIG: &str = "960ec4";
    const GEN_OUTPUT: &str = "bazel-out/k8/bin/gen/gen.out";
    const EXEC_GEN_OUTPUT: &str = "bazel-out/k8-exec/bin/gen/gen.out";
    const DOCS_OUTPUT: &str = "bazel-out/k8/bin/docs/site.out";

    fn target(label: &str) -> ConfiguredTarget {
        ConfiguredTarget::new(label.into(), Some(CONFIG.into()))
    }

    fn exec_target(label: &str) -> ConfiguredTarget {
        ConfiguredTarget::new(label.into(), Some(EXEC_CONFIG.into()))
    }

    fn action(primary_output: &str) -> Cause {
        Cause::Action {
            primary_output: primary_output.into(),
        }
    }

    fn failed(causes: &[Cause]) -> BuildOutcome {
        BuildOutcome::Failed {
            causes: causes.to_vec(),
        }
    }

    fn failed_action(owner: ConfiguredTarget, exit_code: i32, message: &str) -> FailedAction {
        FailedAction {
            owner: Some(owner),
            mnemonic: "Genrule".into(),
            exit_code: NonZeroI32::new(exit_code),
            message: message.into(),
        }
    }

    /// A test that ran once, in one piece, for 0.3 seconds.
    pub(super) fn test(status: TestStatus) -> TestOutcome {
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

    /// A graph of the packages `//gen`, `//app` and `//lib`: `//gen:gen` with
    /// the action writing [`GEN_OUTPUT`], `//app:bin` and `//app:check`, each
    /// configured under [`CONFIG`], and `//lib:broken`, which is not
    /// configured.
    struct Built {
        graph: Graph,
        names: Names,
    }

    impl Built {
        fn new() -> Self {
            let mut built = Built {
                graph: Graph::new(),
                names: Names::default(),
            };
            for (package, target) in [("gen", "gen"), ("app", "bin"), ("app", "check")] {
                let configured = built.target(package, target, true);
                if package == "gen" {
                    let action = format!("action:{GEN_OUTPUT}");
                    built.node(&action, Kind::Action, Some(configured));
                }
            }
            built.target("lib", "broken", false);
            built
        }

        fn node(&mut self, name: &str, kind: Kind, parent: Option<Id>) -> Id {
            match self.names.get(name) {
                Some(id) => id,
                None => {
                    let id = self.names.intern(name);
                    self.graph.add_node(Node::new(id, name, kind, parent));
                    id
                }
            }
        }

        /// The innermost node of `//{package}:{name}`: the configured target
        /// where it is `configured`, the target where not.
        fn target(&mut self, package: &str, name: &str, configured: bool) -> Id {
            let label = format!("//{package}:{name}");
            let package = self.node(&format!("pkg:{package}"), Kind::Package, None);
            let target = self.node(&format!("target:{label}"), Kind::Target, Some(package));
            match configured {
                true => {
                    let name = format!("ct:{label}#{CONFIG}");
                    self.node(&name, Kind::ConfiguredTarget, Some(target))
                }
                false => target,
            }
        }

        fn id(&self, name: &str) -> Id {
            self.names.get(name).expect("a node of the graph")
        }
    }

    /// The two configurations the fixtures build under, by their mnemonics.
    fn configurations() -> Configurations {
        let mut configurations = Configurations::default();
        configurations.insert(CONFIG.into(), "k8-fastbuild".into());
        configurations.insert(EXEC_CONFIG.into(), "k8-opt-exec".into());
        configurations
    }

    fn report(outcomes: &Outcomes) -> Report {
        let built = Built::new();
        let nodes = Nodes::new(&built.graph, &built.names, outcomes);
        Report::of(&nodes)
    }

    /// Each row `outcomes` lists as it reads: its first line, its second,
    /// and whether a click on it shows a node.
    fn rows(outcomes: &Outcomes) -> Vec<(String, String, bool)> {
        let (report, configurations) = (report(outcomes), configurations());
        let read = |row: &Row| {
            let subject = row.subject(&configurations);
            let name = match row.lead() {
                Some(lead) => format!("{lead} {subject}"),
                None => subject,
            };
            (name, row.detail(), row.shown.node().is_some())
        };
        report.listed(0..report.rows()).map(read).collect()
    }

    fn read(name: &str, detail: &str, shown: bool) -> (String, String, bool) {
        (name.to_owned(), detail.to_owned(), shown)
    }

    #[test]
    fn the_targets_are_tallied_by_how_building_them_ended_leaving_out_what_none_did() {
        let mut outcomes = Outcomes::default();
        let line = |outcomes: &Outcomes| report(outcomes).builds.line();
        assert_eq!(line(&outcomes), "0 targets");

        outcomes.report_build(target("//app:bin"), BuildOutcome::Built);
        assert_eq!(line(&outcomes), "1 target: 1 built");

        outcomes.report_build(target("//app:cli"), BuildOutcome::Built);
        outcomes.report_build(target("//gen:gen"), failed(&[]));
        outcomes.report_build(
            target("//lib:broken"),
            BuildOutcome::AnalysisFailed { causes: Vec::new() },
        );
        outcomes.report_build(target("//app:late"), BuildOutcome::NotBuilt);
        assert_eq!(line(&outcomes), "5 targets: 2 built, 2 failed, 1 not built");

        outcomes.report_build(target("//app:windows"), BuildOutcome::Skipped);
        assert_eq!(
            line(&outcomes),
            "6 targets: 2 built, 2 failed, 1 not built, 1 skipped"
        );
    }

    fn mismatches_of(outcomes: &Outcomes) -> Vec<String> {
        report(outcomes).mismatches.lines().collect()
    }

    #[test]
    fn a_graph_holding_everything_reported_as_reported_has_no_mismatch() {
        let mut outcomes = Outcomes::default();
        outcomes.report_build(target("//app:bin"), BuildOutcome::Built);
        // A graph that configures a target not at all holds it for every
        // configuration, and one reported under none is held by its target.
        outcomes.report_build(target("//lib:broken"), BuildOutcome::Built);
        outcomes.report_build(
            ConfiguredTarget::new("//app:check".into(), None),
            BuildOutcome::NotBuilt,
        );
        outcomes.report_failed_action(GEN_OUTPUT.into(), failed_action(target("//gen:gen"), 1, ""));
        // The graph holds no such action, and holds its owner.
        outcomes.report_failed_action(
            "bazel-out/k8/bin/app/bin.o".into(),
            failed_action(target("//app:bin"), 1, ""),
        );
        assert_eq!(mismatches_of(&outcomes), Vec::<String>::new());
    }

    #[test]
    fn what_the_graph_does_not_hold_is_said_of_how_many() {
        let mut outcomes = Outcomes::default();
        outcomes.report_build(target("//app:bin"), BuildOutcome::Built);
        outcomes.report_build(target("//docs:site"), BuildOutcome::Built);
        outcomes.report_failed_action(
            DOCS_OUTPUT.into(),
            failed_action(target("//docs:site"), 1, ""),
        );
        assert_eq!(
            mismatches_of(&outcomes),
            [
                "1 of 2 reported targets is not in this graph",
                "1 of 1 failed action is not in this graph",
            ]
        );

        outcomes.report_build(target("//docs:index"), BuildOutcome::Built);
        outcomes.report_failed_action(
            "bazel-out/k8/bin/docs/index.out".into(),
            FailedAction {
                owner: None,
                ..failed_action(target("//docs:index"), 1, "")
            },
        );
        assert_eq!(
            mismatches_of(&outcomes),
            [
                "2 of 3 reported targets are not in this graph",
                "2 of 2 failed actions are not in this graph",
            ]
        );
    }

    #[test]
    fn a_target_the_graph_configures_otherwise_is_counted_apart_from_one_it_lacks() {
        let mut outcomes = Outcomes::default();
        outcomes.report_build(target("//app:bin"), BuildOutcome::Built);
        outcomes.report_build(exec_target("//app:bin"), BuildOutcome::Built);
        outcomes.report_build(exec_target("//app:check"), BuildOutcome::Built);
        outcomes.report_build(exec_target("//docs:site"), BuildOutcome::Built);
        assert_eq!(
            mismatches_of(&outcomes),
            [
                "1 of 4 reported targets is not in this graph",
                "2 of 4 reported targets are in this graph under other configurations only",
            ]
        );
    }

    #[test]
    fn a_failed_action_is_listed_with_how_it_exited_and_what_it_failed() {
        let mut outcomes = Outcomes::default();
        outcomes.report_failed_action(
            GEN_OUTPUT.into(),
            failed_action(
                target("//gen:gen"),
                1,
                "bash failed: exit 1\n\nUse --sandbox_debug",
            ),
        );
        for label in ["//gen:gen", "//app:bin"] {
            outcomes.report_build(target(label), failed(&[action(GEN_OUTPUT)]));
        }

        assert_eq!(
            rows(&outcomes),
            [read(
                "Genrule //gen:gen",
                "exited 1, failing 2 targets — bash failed: exit 1",
                true
            )]
        );
        let report = report(&outcomes);
        assert_eq!(
            report.root_causes[0].hover(&configurations()),
            "Genrule //gen:gen\nexited 1, failing 2 targets\n\nbash failed: exit 1\n\nUse --sandbox_debug"
        );
        assert_eq!(
            report.root_causes[0].shown.node(),
            Some(Built::new().id(&format!("action:{GEN_OUTPUT}")))
        );
    }

    #[test]
    fn the_root_causes_are_listed_from_the_one_failing_the_most_targets() {
        let mut outcomes = Outcomes::default();
        outcomes.report_failed_action(
            DOCS_OUTPUT.into(),
            failed_action(target("//docs:site"), 2, ""),
        );
        outcomes.report_failed_action(GEN_OUTPUT.into(), failed_action(target("//gen:gen"), 1, ""));
        outcomes.report_build(target("//docs:site"), failed(&[action(DOCS_OUTPUT)]));
        for label in ["//gen:gen", "//app:bin", "//app:cli"] {
            outcomes.report_build(target(label), failed(&[action(GEN_OUTPUT)]));
        }
        outcomes.report_build(target("//app:late"), failed(&[]));

        assert_eq!(
            rows(&outcomes),
            [
                read("Genrule //gen:gen", "exited 1, failing 3 targets", true),
                read(
                    "Genrule //docs:site",
                    "exited 2, failing 1 target · not in this graph",
                    false
                ),
                read(
                    "//app:late",
                    "failed, and the build names no cause · not in this graph",
                    false
                ),
            ]
        );
    }

    /// The order of root causes blamed alike is theirs, whatever order the
    /// build's reports are walked in.
    #[test]
    fn root_causes_failing_as_many_targets_are_listed_in_their_own_order() {
        let mut outcomes = Outcomes::default();
        let outputs: Vec<String> = (0..8)
            .map(|nth| format!("bazel-out/k8/bin/docs/part{nth}.out"))
            .collect();
        for (nth, output) in outputs.iter().enumerate() {
            let causes = [
                action(output),
                Cause::Target(target(&format!("//lib:missing{nth}"))),
            ];
            outcomes.report_build(target(&format!("//docs:part{nth}")), failed(&causes));
        }
        let listed: Vec<String> = rows(&outcomes).into_iter().map(|(name, ..)| name).collect();
        let actions = outputs
            .iter()
            .map(|output| format!("action writing {output}"));
        let targets = (0..8).map(|nth| format!("//lib:missing{nth}"));
        assert_eq!(listed, actions.chain(targets).collect::<Vec<_>>());
    }

    /// A build ending at its first failure reports the action without the
    /// targets it failed, and one run without action events reports a cause
    /// without the action.
    #[test]
    fn an_action_is_listed_whichever_of_its_failure_and_its_blame_is_reported() {
        let mut unblamed = Outcomes::default();
        unblamed.report_failed_action(GEN_OUTPUT.into(), failed_action(target("//gen:gen"), 1, ""));
        assert_eq!(
            rows(&unblamed),
            [read("Genrule //gen:gen", "exited 1", true)]
        );

        let mut unowned = Outcomes::default();
        unowned.report_failed_action(
            GEN_OUTPUT.into(),
            FailedAction {
                owner: None,
                mnemonic: String::new(),
                exit_code: None,
                message: String::new(),
            },
        );
        assert_eq!(
            rows(&unowned),
            [read(
                "action writing bazel-out/k8/bin/gen/gen.out",
                "failed",
                true
            )]
        );
    }

    /// A target failing through nothing the graph holds stands as the root
    /// cause itself, so that is the node the row of what it failed through
    /// shows.
    #[test]
    fn a_root_cause_the_graph_does_not_hold_shows_the_first_target_it_failed() {
        let mut outcomes = Outcomes::default();
        for label in ["//app:check", "//app:bin", "//docs:site"] {
            outcomes.report_build(target(label), failed(&[action(DOCS_OUTPUT)]));
        }
        let report = report(&outcomes);
        assert_eq!(
            rows(&outcomes),
            [read(
                "action writing bazel-out/k8/bin/docs/site.out",
                "failed, failing 3 targets · not in this graph",
                true
            )]
        );
        assert_eq!(
            report.root_causes[0].shown.node(),
            Some(Built::new().id(&format!("ct://app:bin#{CONFIG}")))
        );

        let mut elsewhere = Outcomes::default();
        elsewhere.report_build(target("//docs:site"), failed(&[action(DOCS_OUTPUT)]));
        assert_eq!(
            rows(&elsewhere),
            [read(
                "action writing bazel-out/k8/bin/docs/site.out",
                "failed, failing 1 target · not in this graph",
                false
            )]
        );
    }

    #[test]
    fn root_causes_named_alike_under_two_configurations_name_theirs() {
        let mut outcomes = Outcomes::default();
        outcomes.report_failed_action(GEN_OUTPUT.into(), failed_action(target("//gen:gen"), 1, ""));
        outcomes.report_failed_action(
            EXEC_GEN_OUTPUT.into(),
            failed_action(exec_target("//gen:gen"), 1, ""),
        );
        outcomes.report_failed_action(
            DOCS_OUTPUT.into(),
            failed_action(target("//docs:site"), 2, ""),
        );
        let unnamed = ConfiguredTarget::new("//lib:broken".to_owned(), Some("0c3d9a77".into()));
        for broken in [target("//lib:broken"), unnamed] {
            outcomes.report_build(broken, BuildOutcome::AnalysisFailed { causes: Vec::new() });
        }
        let named: Vec<String> = rows(&outcomes).into_iter().map(|(name, ..)| name).collect();
        assert_eq!(
            named,
            [
                "Genrule //gen:gen (k8-opt-exec)",
                "Genrule //docs:site",
                "Genrule //gen:gen (k8-fastbuild)",
                "//lib:broken (0c3d9a)",
                "//lib:broken (k8-fastbuild)",
            ]
        );
    }

    #[test]
    fn a_target_that_could_not_be_analysed_is_listed_with_what_the_build_says_of_it() {
        let unconfigured =
            |label: &str| -> ConfiguredTarget { ConfiguredTarget::new(label.into(), None) };
        let blamed = |outcomes: &mut Outcomes, cause: &ConfiguredTarget| {
            for label in ["//app:cli", "//app:bin"] {
                outcomes.report_build(
                    unconfigured(label),
                    BuildOutcome::AnalysisFailed {
                        causes: vec![Cause::Target(cause.clone())],
                    },
                );
            }
        };
        let mut outcomes = Outcomes::default();
        blamed(&mut outcomes, &target("//lib:broken"));
        outcomes.report_target_error(
            target("//lib:broken"),
            "no such attribute 'dep'\nin cc_library rule".into(),
        );
        assert_eq!(
            rows(&outcomes),
            [read(
                "//lib:broken",
                "could not be loaded or analysed, failing 2 targets — no such attribute 'dep'",
                true
            )]
        );

        let mut uncaused = Outcomes::default();
        uncaused.report_build(
            unconfigured("//lib:broken"),
            BuildOutcome::AnalysisFailed { causes: Vec::new() },
        );
        assert_eq!(
            rows(&uncaused),
            [read("//lib:broken", "could not be analysed", true)]
        );
    }

    #[test]
    fn the_tests_are_tallied_by_how_running_them_ended_leaving_out_what_none_did() {
        let mut outcomes = Outcomes::default();
        let line = |outcomes: &Outcomes| report(outcomes).tests.line();
        outcomes.report_build(target("//app:bin"), BuildOutcome::Built);
        assert_eq!(line(&outcomes), None);

        outcomes.report_test(target("//app:check"), test(TestStatus::Passed));
        assert_eq!(line(&outcomes).as_deref(), Some("1 test: 1 passed"));

        let cached = |actions| TestOutcome {
            actions,
            cached_actions: 2,
            shards: NonZeroU32::new(2),
            ..test(TestStatus::Passed)
        };
        outcomes.report_test(target("//app:cached"), cached(2));
        outcomes.report_test(target("//app:half_cached"), cached(3));
        assert_eq!(
            line(&outcomes).as_deref(),
            Some("3 tests: 3 passed (1 cached)")
        );

        for (label, status) in [
            ("//app:flaky", TestStatus::Flaky),
            ("//app:failing", TestStatus::Failed),
            ("//app:slow", TestStatus::Timeout),
            ("//app:slower", TestStatus::Timeout),
            ("//app:remote", TestStatus::RemoteFailure),
            ("//app:partial", TestStatus::Incomplete),
            ("//app:unrun", TestStatus::NoStatus),
            ("//app:halted", TestStatus::ToolHaltedBeforeTesting),
            ("//app:unbuilt", TestStatus::FailedToBuild),
        ] {
            outcomes.report_test(target(label), test(status));
        }
        assert_eq!(
            line(&outcomes).as_deref(),
            Some(
                "12 tests: 3 passed (1 cached), 1 flaky, 1 failed, 2 timed out, \
                 1 failed remotely, 1 incomplete, 2 not run, 1 not built"
            )
        );
    }

    #[test]
    fn the_tests_that_did_not_pass_are_listed_worst_first_under_the_root_causes() {
        let mut outcomes = Outcomes::default();
        outcomes.report_failed_action(GEN_OUTPUT.into(), failed_action(target("//gen:gen"), 1, ""));
        for (label, status) in [
            ("//app:unrun", TestStatus::NoStatus),
            ("//app:flaky", TestStatus::Flaky),
            ("//app:passing", TestStatus::Passed),
            ("//app:slow", TestStatus::Timeout),
            ("//app:unbuilt", TestStatus::FailedToBuild),
            ("//app:check", TestStatus::Failed),
            ("//app:broken", TestStatus::Failed),
        ] {
            outcomes.report_test(target(label), test(status));
        }

        let report = report(&outcomes);

        assert_eq!(
            rows(&outcomes),
            [
                read("Genrule //gen:gen", "exited 1", true),
                read("//app:broken", "failed in 0.3s · not in this graph", false),
                read("//app:check", "failed in 0.3s", true),
                read(
                    "//app:slow",
                    "timed out after 0.3s · not in this graph",
                    false
                ),
                read(
                    "//app:flaky",
                    "flaky: passed on attempt 1 in 0.3s · not in this graph",
                    false
                ),
                read(
                    "//app:unrun",
                    "no status: the test did not run · not in this graph",
                    false
                ),
            ]
        );
        let listed = report.listed(0..report.rows());
        let badges: Vec<Status> = listed.map(|row| row.listed.status()).collect();
        assert_eq!(
            badges,
            [
                Status::RootCause,
                Status::TestFailed,
                Status::TestFailed,
                Status::TestFailed,
                Status::Flaky,
                Status::Unfinished,
            ]
        );
    }

    #[test]
    fn a_test_listed_under_two_configurations_names_each() {
        let mut outcomes = Outcomes::default();
        outcomes.report_test(target("//app:check"), test(TestStatus::Failed));
        outcomes.report_test(exec_target("//app:check"), test(TestStatus::Timeout));
        outcomes.report_test(target("//app:other"), test(TestStatus::Failed));
        let named: Vec<String> = rows(&outcomes).into_iter().map(|(name, ..)| name).collect();
        assert_eq!(
            named,
            [
                "//app:check (k8-fastbuild)",
                "//app:other",
                "//app:check (k8-opt-exec)",
            ]
        );
    }

    #[test]
    fn a_build_whose_tests_did_not_all_pass_is_one_to_look_into() {
        let mut outcomes = Outcomes::default();
        outcomes.report_test(target("//app:check"), test(TestStatus::Passed));
        let passed = report(&outcomes);
        assert!(!passed.is_empty() && !passed.troubled());

        outcomes.report_test(target("//app:flaky"), test(TestStatus::Flaky));
        assert!(report(&outcomes).troubled());
    }

    /// The report of a build that only built, and of one that failed.
    pub(super) fn quiet_and_troubled() -> (Report, Report) {
        let mut outcomes = Outcomes::default();
        outcomes.report_build(target("//app:bin"), BuildOutcome::Built);
        let quiet = report(&outcomes);
        outcomes.report_build(target("//gen:gen"), failed(&[]));
        (quiet, report(&outcomes))
    }

    #[test]
    fn a_build_reporting_a_failure_is_one_to_look_into() {
        let mut outcomes = Outcomes::default();
        let unreported = report(&outcomes);
        assert!(unreported.is_empty() && !unreported.troubled());

        outcomes.report_build(target("//app:bin"), BuildOutcome::Built);
        outcomes.report_build(target("//app:windows"), BuildOutcome::Skipped);
        let succeeded = report(&outcomes);
        assert!(!succeeded.is_empty() && !succeeded.troubled());

        let mut unfinished = Outcomes::default();
        unfinished.report_build(target("//app:bin"), BuildOutcome::NotBuilt);
        assert!(report(&unfinished).troubled());

        let mut failed = Outcomes::default();
        failed.report_failed_action(GEN_OUTPUT.into(), failed_action(target("//gen:gen"), 1, ""));
        let failed = report(&failed);
        assert!(!failed.is_empty() && failed.troubled());
    }
}
