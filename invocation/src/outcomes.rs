//! What a build reports of the targets it was asked for, the tests it ran and
//! the actions that failed.

use std::collections::HashMap;
use std::num::{NonZeroI32, NonZeroU32};
use std::time::Duration;

/// A target in one configuration, as a build names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConfiguredTarget<S = String> {
    label: S,
    /// The configuration's checksum, which is never empty.
    configuration: Option<S>,
}

impl<S: AsRef<str>> ConfiguredTarget<S> {
    /// `label` in the configuration whose checksum is `configuration`. No
    /// configuration, which an empty checksum names too, is that of a target
    /// that has none, such as a source file, and of one a build names before
    /// configuring it.
    pub fn new(label: S, configuration: Option<S>) -> Self {
        Self {
            label,
            configuration: configuration.filter(|checksum| !checksum.as_ref().is_empty()),
        }
    }
}

impl ConfiguredTarget {
    pub fn borrowed(&self) -> ConfiguredTarget<&str> {
        ConfiguredTarget {
            label: &self.label,
            configuration: self.configuration.as_deref(),
        }
    }
}

impl<'a> ConfiguredTarget<&'a str> {
    pub fn label(self) -> &'a str {
        self.label
    }

    pub fn configuration(self) -> Option<&'a str> {
        self.configuration
    }

    pub fn owned(self) -> ConfiguredTarget {
        ConfiguredTarget {
            label: self.label.to_owned(),
            configuration: self.configuration.map(str::to_owned),
        }
    }
}

/// What the failure of a target traces back to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cause {
    /// An action that failed, named by the primary output
    /// [`Outcomes::failed_action`] finds it by.
    Action { primary_output: String },
    /// A target that could not be loaded or analysed, of which
    /// [`Outcomes::target_error`] says why.
    Target(ConfiguredTarget),
}

/// How the build of one target ended, which covers the target and the aspects
/// applied to it. Running a test is no part of building it: that is the
/// test's [`TestOutcome`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildOutcome {
    Built,
    /// Left out of the build as incompatible with the target platform.
    Skipped,
    /// The build ended before the target was built.
    NotBuilt,
    Failed {
        causes: Vec<Cause>,
    },
    AnalysisFailed {
        causes: Vec<Cause>,
    },
}

impl BuildOutcome {
    /// The rank the outcomes reported of one target are compared by.
    fn severity(&self) -> u8 {
        match self {
            BuildOutcome::Built => 0,
            BuildOutcome::Skipped => 1,
            BuildOutcome::NotBuilt => 2,
            BuildOutcome::Failed { .. } => 3,
            BuildOutcome::AnalysisFailed { .. } => 4,
        }
    }

    fn causes_mut(&mut self) -> Option<&mut Vec<Cause>> {
        match self {
            BuildOutcome::Failed { causes } | BuildOutcome::AnalysisFailed { causes } => {
                Some(causes)
            }
            BuildOutcome::Built | BuildOutcome::Skipped | BuildOutcome::NotBuilt => None,
        }
    }

    fn combine(&mut self, mut other: BuildOutcome) {
        if other.severity() > self.severity() {
            std::mem::swap(self, &mut other);
        }
        if let (Some(causes), Some(more)) = (self.causes_mut(), other.causes_mut()) {
            causes.reserve(more.len());
            for cause in more.drain(..) {
                if !causes.contains(&cause) {
                    causes.push(cause);
                }
            }
        }
    }
}

/// A test's status over all its runs, shards and attempts: Bazel's
/// `build_event_stream.TestStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestStatus {
    NoStatus,
    Passed,
    Flaky,
    Timeout,
    Failed,
    Incomplete,
    RemoteFailure,
    FailedToBuild,
    ToolHaltedBeforeTesting,
}

/// How running one test ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestOutcome {
    pub status: TestStatus,
    /// The most attempts a shard of one run took.
    pub attempts: u32,
    /// Times the test was to run (`--runs_per_test`).
    pub runs: u32,
    /// `None` for a test that is not sharded.
    pub shards: Option<NonZeroU32>,
    /// Test actions over every run, shard and attempt.
    pub actions: u32,
    /// The [`actions`](Self::actions) whose result came from a cache.
    pub cached_actions: u32,
    pub run_duration: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedAction {
    /// The target the action belongs to, where the build names it.
    pub owner: Option<ConfiguredTarget>,
    pub mnemonic: String,
    /// The code the action's process exited with. `None` where the build
    /// states none, as of a failure that is no process exiting: a timeout, an
    /// output the action did not write.
    pub exit_code: Option<NonZeroI32>,
    pub message: String,
}

/// A value for each configured target reported on. The configurations of one
/// label are few and share the label's entry, which a lookup reaches by a
/// borrowed label.
pub(crate) struct PerTarget<T>(HashMap<String, Vec<(Option<String>, T)>>);

impl<T> Default for PerTarget<T> {
    fn default() -> Self {
        Self(HashMap::new())
    }
}

impl<T> PerTarget<T> {
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn get(&self, target: ConfiguredTarget<&str>) -> Option<&T> {
        let configurations = self.0.get(target.label)?;
        configurations
            .iter()
            .find(|(configuration, _)| configuration.as_deref() == target.configuration)
            .map(|(_, value)| value)
    }

    /// Holds `value` for `target`, or has `combine` fold it into the value
    /// `target` already has.
    pub(crate) fn report(
        &mut self,
        target: ConfiguredTarget,
        value: T,
        combine: impl FnOnce(&mut T, T),
    ) {
        let configurations = self.0.entry(target.label).or_default();
        let reported = configurations
            .iter_mut()
            .find(|(configuration, _)| *configuration == target.configuration);
        match reported {
            Some((_, reported)) => combine(reported, value),
            None => configurations.push((target.configuration, value)),
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (ConfiguredTarget<&str>, &T)> {
        self.0.iter().flat_map(|(label, configurations)| {
            configurations.iter().map(move |(configuration, value)| {
                let target = ConfiguredTarget {
                    label: label.as_str(),
                    configuration: configuration.as_deref(),
                };
                (target, value)
            })
        })
    }
}

/// A build reports only on the targets it was asked for, so of most of a build
/// graph nothing is reported: the `None` of a lookup, which is none of the
/// outcomes a report can state.
#[derive(Default)]
pub struct Outcomes {
    builds: PerTarget<BuildOutcome>,
    tests: PerTarget<TestOutcome>,
    failed_actions: HashMap<String, FailedAction>,
    target_errors: PerTarget<String>,
}

impl Outcomes {
    /// Whether the build reported nothing at all.
    pub fn is_empty(&self) -> bool {
        self.builds.0.is_empty()
            && self.tests.0.is_empty()
            && self.failed_actions.is_empty()
            && self.target_errors.0.is_empty()
    }

    /// A target whose analysis failed before it was configured is reported
    /// under no configuration.
    pub fn build(&self, target: ConfiguredTarget<&str>) -> Option<&BuildOutcome> {
        self.builds.get(target)
    }

    /// A test's outcome is reported under the test's own label, and its build
    /// outcome under the label the build was asked for: for a test asked for
    /// through an alias the two differ.
    pub fn test(&self, target: ConfiguredTarget<&str>) -> Option<&TestOutcome> {
        self.tests.get(target)
    }

    /// The failed action whose primary output has the exec path
    /// `primary_output`.
    pub fn failed_action(&self, primary_output: &str) -> Option<&FailedAction> {
        self.failed_actions.get(primary_output)
    }

    /// What the build says went wrong loading or analysing `target`.
    pub fn target_error(&self, target: ConfiguredTarget<&str>) -> Option<&str> {
        self.target_errors.get(target).map(String::as_str)
    }

    pub fn builds(&self) -> impl Iterator<Item = (ConfiguredTarget<&str>, &BuildOutcome)> {
        self.builds.iter()
    }

    pub fn tests(&self) -> impl Iterator<Item = (ConfiguredTarget<&str>, &TestOutcome)> {
        self.tests.iter()
    }

    /// Each failed action with the exec path of its primary output.
    pub fn failed_actions(&self) -> impl Iterator<Item = (&str, &FailedAction)> {
        self.failed_actions
            .iter()
            .map(|(primary_output, action)| (primary_output.as_str(), action))
    }

    /// Reports how building `target`, or applying an aspect to it, ended. The
    /// target's outcome is the most severe of what is reported of it, and
    /// traces back to the causes of every failure reported of it.
    pub fn report_build(&mut self, target: ConfiguredTarget, outcome: BuildOutcome) {
        self.builds.report(target, outcome, BuildOutcome::combine);
    }

    /// Reports how running the test `target` ended. A build summarises a test
    /// once, so a test already reported keeps the outcome it has.
    pub fn report_test(&mut self, target: ConfiguredTarget, outcome: TestOutcome) {
        self.tests.report(target, outcome, |_, _| {});
    }

    /// Reports the failure of the action whose primary output has the exec
    /// path `primary_output`. An action fails once, so one already reported
    /// keeps the failure it has.
    pub fn report_failed_action(&mut self, primary_output: String, action: FailedAction) {
        self.failed_actions.entry(primary_output).or_insert(action);
    }

    /// Reports what went wrong loading or analysing `target`. A target
    /// already reported on keeps the error it has.
    pub fn report_target_error(&mut self, target: ConfiguredTarget, error: String) {
        self.target_errors.report(target, error, |_, _| {});
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target<'a>(label: &'a str, configuration: Option<&'a str>) -> ConfiguredTarget<&'a str> {
        ConfiguredTarget::new(label, configuration)
    }

    fn owned(label: &str, configuration: Option<&str>) -> ConfiguredTarget {
        ConfiguredTarget::new(label.into(), configuration.map(Into::into))
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

    fn analysis_failed(causes: &[Cause]) -> BuildOutcome {
        BuildOutcome::AnalysisFailed {
            causes: causes.to_vec(),
        }
    }

    fn test_outcome(status: TestStatus) -> TestOutcome {
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

    fn failed_action(message: &str) -> FailedAction {
        FailedAction {
            owner: Some(owned("//codegen:gen", Some("a7a71f"))),
            mnemonic: "Genrule".into(),
            exit_code: NonZeroI32::new(2),
            message: message.into(),
        }
    }

    #[test]
    fn nothing_reported_is_none_of_the_outcomes() {
        let mut outcomes = Outcomes::default();
        assert_eq!(outcomes.build(target("//app", Some("a7a71f"))), None);
        assert_eq!(outcomes.test(target("//app", Some("a7a71f"))), None);
        assert_eq!(outcomes.failed_action("bazel-out/bin/app"), None);
        assert_eq!(outcomes.target_error(target("//app", Some("a7a71f"))), None);

        outcomes.report_build(owned("//app", Some("a7a71f")), BuildOutcome::Built);
        assert_eq!(outcomes.build(target("//lib", Some("a7a71f"))), None);
    }

    #[test]
    fn any_one_report_leaves_the_outcomes_non_empty() {
        assert!(Outcomes::default().is_empty());
        let app = || owned("//app", Some("a7a71f"));
        let reports: [fn(&mut Outcomes, ConfiguredTarget); 4] = [
            |outcomes, app| outcomes.report_build(app, BuildOutcome::Built),
            |outcomes, app| outcomes.report_test(app, test_outcome(TestStatus::Passed)),
            |outcomes, _| {
                outcomes.report_failed_action("bazel-out/bin/gen.out".into(), failed_action(""))
            },
            |outcomes, app| outcomes.report_target_error(app, "not visible".into()),
        ];
        for (nth, report) in reports.into_iter().enumerate() {
            let mut outcomes = Outcomes::default();
            report(&mut outcomes, app());
            assert!(!outcomes.is_empty(), "report {nth}");
        }
    }

    #[test]
    fn an_empty_checksum_names_no_configuration() {
        assert_eq!(target("//app", Some("")), target("//app", None));
        assert_eq!(target("//app", Some("")).configuration(), None);
        assert_eq!(owned("//app", Some("")), owned("//app", None));
        assert_ne!(target("//app", Some("a7a71f")), target("//app", None));

        let mut outcomes = Outcomes::default();
        outcomes.report_build(owned("//app", Some("")), BuildOutcome::Built);
        assert_eq!(
            outcomes.build(target("//app", None)),
            Some(&BuildOutcome::Built)
        );
    }

    #[test]
    fn a_target_has_an_outcome_per_configuration() {
        let mut outcomes = Outcomes::default();
        outcomes.report_build(owned("//app", Some("a7a71f")), BuildOutcome::Built);
        outcomes.report_build(owned("//app", Some("0c3d9a")), BuildOutcome::Skipped);
        outcomes.report_build(owned("//app", None), BuildOutcome::NotBuilt);

        let build = |configuration| outcomes.build(target("//app", configuration));
        assert_eq!(build(Some("a7a71f")), Some(&BuildOutcome::Built));
        assert_eq!(build(Some("0c3d9a")), Some(&BuildOutcome::Skipped));
        assert_eq!(build(None), Some(&BuildOutcome::NotBuilt));
        assert_eq!(build(Some("ffffff")), None);

        let mut reported: Vec<_> = outcomes.builds().collect();
        reported.sort_by_key(|(target, _)| target.configuration());
        assert_eq!(
            reported,
            [
                (target("//app", None), &BuildOutcome::NotBuilt),
                (target("//app", Some("0c3d9a")), &BuildOutcome::Skipped),
                (target("//app", Some("a7a71f")), &BuildOutcome::Built),
            ]
        );
    }

    #[test]
    fn a_build_and_a_test_outcome_of_one_target_are_apart() {
        let mut outcomes = Outcomes::default();
        let test = owned("//app:test", Some("a7a71f"));
        outcomes.report_build(test.clone(), BuildOutcome::Built);
        assert_eq!(outcomes.test(test.borrowed()), None);

        outcomes.report_test(test.clone(), test_outcome(TestStatus::Failed));
        assert_eq!(outcomes.build(test.borrowed()), Some(&BuildOutcome::Built));
        assert_eq!(
            outcomes.tests().collect::<Vec<_>>(),
            [(test.borrowed(), &test_outcome(TestStatus::Failed))]
        );
    }

    #[test]
    fn the_most_severe_report_of_a_target_is_its_outcome() {
        let ascending = [
            BuildOutcome::Built,
            BuildOutcome::Skipped,
            BuildOutcome::NotBuilt,
            failed(&[]),
            analysis_failed(&[]),
        ];
        for (rank, milder) in ascending.iter().enumerate() {
            for severer in &ascending[rank..] {
                for (first, second) in [(milder, severer), (severer, milder)] {
                    let mut outcomes = Outcomes::default();
                    outcomes.report_build(owned("//app", None), first.clone());
                    outcomes.report_build(owned("//app", None), second.clone());
                    assert_eq!(
                        outcomes.build(target("//app", None)),
                        Some(severer),
                        "{first:?} then {second:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn failures_of_one_target_pool_their_causes() {
        let mut outcomes = Outcomes::default();
        let app = owned("//app", Some("a7a71f"));
        let gen_out = action("bazel-out/bin/gen.out");
        let lint_out = action("bazel-out/bin/app.lint");
        outcomes.report_build(app.clone(), failed(std::slice::from_ref(&gen_out)));
        outcomes.report_build(app.clone(), failed(&[lint_out.clone(), gen_out.clone()]));
        assert_eq!(
            outcomes.build(app.borrowed()),
            Some(&failed(&[gen_out, lint_out]))
        );

        let broken = Cause::Target(owned("//lib", Some("a7a71f")));
        let missing = Cause::Target(owned("//lib:missing.h", None));
        let lib = owned("//lib:all", None);
        outcomes.report_build(lib.clone(), analysis_failed(std::slice::from_ref(&broken)));
        outcomes.report_build(lib.clone(), analysis_failed(std::slice::from_ref(&missing)));
        assert_eq!(
            outcomes.build(lib.borrowed()),
            Some(&analysis_failed(&[broken, missing]))
        );
    }

    #[test]
    fn an_analysis_failure_keeps_the_causes_of_a_failure_of_the_same_target() {
        let gen_out = action("bazel-out/bin/gen.out");
        let conflict = Cause::Target(owned("//app", Some("a7a71f")));
        let reports = [
            failed(std::slice::from_ref(&gen_out)),
            analysis_failed(std::slice::from_ref(&conflict)),
        ];
        for (first, second) in [(0, 1), (1, 0)] {
            let mut outcomes = Outcomes::default();
            let app = owned("//app", Some("a7a71f"));
            outcomes.report_build(app.clone(), reports[first].clone());
            outcomes.report_build(app.clone(), BuildOutcome::NotBuilt);
            outcomes.report_build(app.clone(), reports[second].clone());
            let Some(BuildOutcome::AnalysisFailed { causes }) = outcomes.build(app.borrowed())
            else {
                panic!("an analysis failure, reported {first} then {second}");
            };
            assert_eq!(causes.len(), 2);
            assert!(causes.contains(&gen_out) && causes.contains(&conflict));
        }
    }

    #[test]
    fn a_target_error_reported_again_keeps_its_first() {
        let mut outcomes = Outcomes::default();
        let private = owned("//core:private", Some("a7a71f"));
        outcomes.report_target_error(private.clone(), "not visible".into());
        outcomes.report_target_error(private.clone(), "second".into());
        assert_eq!(
            outcomes.target_error(private.borrowed()),
            Some("not visible")
        );
        assert_eq!(outcomes.target_error(target("//core:private", None)), None);
    }

    #[test]
    fn a_test_reported_again_keeps_its_first_outcome() {
        let mut outcomes = Outcomes::default();
        let test = owned("//app:test", Some("a7a71f"));
        outcomes.report_test(test.clone(), test_outcome(TestStatus::Flaky));
        outcomes.report_test(test.clone(), test_outcome(TestStatus::Passed));
        assert_eq!(
            outcomes.test(test.borrowed()),
            Some(&test_outcome(TestStatus::Flaky))
        );
    }

    #[test]
    fn a_failed_action_reported_again_keeps_its_first_failure() {
        let mut outcomes = Outcomes::default();
        let output = "bazel-out/bin/gen.out";
        outcomes.report_failed_action(output.into(), failed_action("first"));
        outcomes.report_failed_action(output.into(), failed_action("second"));
        assert_eq!(
            outcomes.failed_action(output),
            Some(&failed_action("first"))
        );
        assert_eq!(
            outcomes.failed_actions().collect::<Vec<_>>(),
            [(output, &failed_action("first"))]
        );
    }
}
