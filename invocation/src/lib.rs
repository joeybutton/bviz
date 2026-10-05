//! The vocabulary shared by the readers of one Bazel invocation's outputs:
//! each output states the same facts in its own encoding, and what the readers
//! return must be comparable to be joined.

use std::collections::HashMap;
use std::sync::Arc;

mod actions;
mod attempts;
mod critical;
mod outcomes;
mod runner;
pub mod spawn;
mod timings;

pub use actions::{ActionGraph, ActionIndex};
pub use attempts::{Attempt, Attempts, Done, Phase, State, Venue};
pub use critical::CriticalStep;
pub use outcomes::{
    BuildOutcome, Cause, ConfiguredTarget, FailedAction, Outcomes, TestOutcome, TestStatus,
};
pub use runner::{Runner, Spawns};
pub use timings::{Profiled, Share, Spent, Timing, Timings};

use spawn::LoggedSpawns;

const MICROS_PER_MILLI: i64 = 1_000;
const MICROS_PER_SECOND: f64 = 1e6;

/// A wall-clock instant, in microseconds since the Unix epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EpochMicros(i64);

impl EpochMicros {
    /// The instant `millis` milliseconds after the Unix epoch, saturating at
    /// the representable range.
    pub const fn from_millis(millis: i64) -> EpochMicros {
        EpochMicros(millis.saturating_mul(MICROS_PER_MILLI))
    }

    /// The instant `seconds` seconds after the Unix epoch, placed as
    /// [`plus_secs_f64`](Self::plus_secs_f64) places it.
    pub fn from_secs_f64(seconds: f64) -> EpochMicros {
        EpochMicros(0).plus_secs_f64(seconds)
    }

    pub const fn micros(self) -> i64 {
        self.0
    }

    /// The instant `micros` microseconds after this one (before it for a
    /// negative count), saturating at the representable range.
    pub const fn plus_micros(self, micros: i64) -> EpochMicros {
        EpochMicros(self.0.saturating_add(micros))
    }

    /// The instant `seconds` seconds after this one, to the nearest
    /// microsecond and saturating at the representable range; this one for a
    /// NaN.
    pub fn plus_secs_f64(self, seconds: f64) -> EpochMicros {
        self.plus_micros((seconds * MICROS_PER_SECOND).round() as i64)
    }
}

/// Bazel's id for one run of a command (its command id), which outputs of that
/// run name alike. Distinct from the build request id, which several
/// invocations can share.
#[derive(Debug, Clone, PartialEq)]
pub struct InvocationId(Arc<str>);

impl InvocationId {
    /// `None` for the empty string, which an output holds in place of an id it
    /// does not know.
    pub fn new(id: &str) -> Option<InvocationId> {
        (!id.is_empty()).then(|| InvocationId(id.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Leading characters of a configuration checksum a reader is shown: what
/// [`Configurations::name`] tells apart configurations sharing a mnemonic by.
const CHECKSUM_SHOWN: usize = 6;

/// The mnemonic of each configuration known of an invocation, keyed by the
/// checksum Bazel's outputs name a configuration by.
#[derive(Default)]
pub struct Configurations(HashMap<String, String>);

impl Configurations {
    pub fn with_capacity(configurations: usize) -> Self {
        Self(HashMap::with_capacity(configurations))
    }

    /// Describes the configuration `checksum` names by its `mnemonic`. A
    /// checksum names the same configuration in every output, so one already
    /// described keeps the description it has.
    pub fn insert(&mut self, checksum: String, mnemonic: String) {
        self.0.entry(checksum).or_insert(mnemonic);
    }

    /// [Describes](Self::insert) every configuration `described` holds.
    pub fn extend(&mut self, described: Configurations) {
        self.0.reserve(described.0.len());
        for (checksum, mnemonic) in described.0 {
            self.insert(checksum, mnemonic);
        }
    }

    /// How to spell the configuration `checksum` names for a reader: its
    /// mnemonic, followed by the start of the checksum when another known
    /// configuration shares the mnemonic, so a name gains that suffix when a
    /// second configuration of its mnemonic becomes known. `None` for an
    /// unknown checksum.
    pub fn name(&self, checksum: &str) -> Option<String> {
        let mnemonic = self.0.get(checksum)?;
        let shared = self
            .0
            .iter()
            .any(|(other, described)| other != checksum && described == mnemonic);
        Some(match shared {
            true => format!("{mnemonic} {}", short(checksum)),
            false => mnemonic.clone(),
        })
    }

    /// [`name`](Self::name), and for an unknown checksum its start.
    pub fn name_or_checksum(&self, checksum: &str) -> String {
        self.name(checksum).unwrap_or_else(|| short(checksum))
    }
}

fn short(checksum: &str) -> String {
    checksum.chars().take(CHECKSUM_SHOWN).collect()
}

/// Which of the reports of one kind a [`Session`] has taken what it holds of
/// that kind from. A reader that keeps the one its reading was taken at can
/// tell whether what it read is still what the session holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Revision(u64);

/// What a [`Session`] holds of one kind of report. An invocation writes one of
/// each output a kind is reported from, so a report stands in place of any
/// taken before, and the revision moves with each report and nothing else.
#[derive(Default)]
struct Reported<T> {
    value: T,
    revision: Revision,
}

impl<T> Reported<T> {
    fn replace(&mut self, value: T) {
        self.value = value;
        self.revision.0 += 1;
    }
}

/// What the views of one invocation's outputs share. Each output's reader adds
/// what it learns of the invocation, for the views of the other outputs to read.
#[derive(Default)]
pub struct Session {
    /// Shared, as a derivation off the UI thread reads it.
    action_graph: Reported<Option<Arc<ActionGraph>>>,
    attempts: Reported<Attempts>,
    /// By its place in `critical_path`.
    chosen_step: Reported<Option<usize>>,
    configurations: Configurations,
    /// In the order they started.
    critical_path: Reported<Vec<CriticalStep>>,
    /// Shared, as a derivation off the UI thread reads it. Held, every
    /// spawn's outputs with it, for as long as the session is, whether or
    /// not a view of the invocation reads them.
    logged_spawns: Reported<Option<Arc<LoggedSpawns>>>,
    outcomes: Reported<Outcomes>,
    timings: Reported<Timings>,
}

impl Session {
    pub fn configurations(&self) -> &Configurations {
        &self.configurations
    }

    /// Adds the configurations one of the invocation's outputs describes.
    pub fn add_configurations(&mut self, described: Configurations) {
        self.configurations.extend(described);
    }

    /// What the invocation's build reported.
    pub fn outcomes(&self) -> &Outcomes {
        &self.outcomes.value
    }

    pub fn outcomes_revision(&self) -> Revision {
        self.outcomes.revision
    }

    pub fn report_outcomes(&mut self, reported: Outcomes) {
        self.outcomes.replace(reported);
    }

    /// What the invocation's profile recorded of its actions.
    pub fn timings(&self) -> &Timings {
        &self.timings.value
    }

    pub fn timings_revision(&self) -> Revision {
        self.timings.revision
    }

    pub fn report_timings(&mut self, reported: Timings) {
        self.timings.replace(reported);
    }

    /// When the invocation's actions were executed; none where its profile
    /// cannot place them on the wall clock.
    pub fn attempts(&self) -> &Attempts {
        &self.attempts.value
    }

    pub fn attempts_revision(&self) -> Revision {
        self.attempts.revision
    }

    pub fn report_attempts(&mut self, reported: Attempts) {
        self.attempts.replace(reported);
    }

    /// The spawns the invocation's execution log records, `None` until one
    /// is reported.
    pub fn logged_spawns(&self) -> Option<&Arc<LoggedSpawns>> {
        self.logged_spawns.value.as_ref()
    }

    pub fn logged_spawns_revision(&self) -> Revision {
        self.logged_spawns.revision
    }

    pub fn report_logged_spawns(&mut self, reported: LoggedSpawns) {
        self.logged_spawns.replace(Some(Arc::new(reported)));
    }

    /// The invocation's action graph, `None` until one is reported.
    pub fn action_graph(&self) -> Option<&Arc<ActionGraph>> {
        self.action_graph.value.as_ref()
    }

    pub fn action_graph_revision(&self) -> Revision {
        self.action_graph.revision
    }

    /// Of several aqueries read as part of the invocation, the last read
    /// stands for its graph, and goes on standing once its document is
    /// closed.
    pub fn report_action_graph(&mut self, reported: ActionGraph) {
        self.action_graph.replace(Some(Arc::new(reported)));
    }

    /// The steps of the invocation's critical path in the order they
    /// started, none until its profile's path is reported.
    pub fn critical_path(&self) -> &[CriticalStep] {
        &self.critical_path.value
    }

    pub fn critical_path_revision(&self) -> Revision {
        self.critical_path.revision
    }

    /// Says whether `reported` differs from the steps held: the same steps
    /// again are no new report, and a new one lets go of the step chosen of
    /// those held.
    pub fn report_critical_path(&mut self, reported: Vec<CriticalStep>) -> bool {
        let changed = self.critical_path.value != reported;
        if changed {
            self.critical_path.replace(reported);
            if self.chosen_step.value.is_some() {
                self.chosen_step.replace(None);
            }
        }
        changed
    }

    /// The step of the [critical path](Self::critical_path) a reader last
    /// chose in a view of one of the invocation's outputs, for the views of
    /// the others to show, by its place among the steps.
    pub fn chosen_step(&self) -> Option<usize> {
        self.chosen_step.value
    }

    /// Changes with every choice, of a step or of none, the same step chosen
    /// again included, so that a view shows each choice once.
    pub fn chosen_step_revision(&self) -> Revision {
        self.chosen_step.revision
    }

    /// Takes `step`, a place among the critical path's steps, for the one a
    /// reader chose, or no step for `None`. Says whether it took it: a place
    /// past the steps holds none to choose.
    pub fn choose_step(&mut self, step: Option<usize>) -> bool {
        if step.is_some_and(|index| index >= self.critical_path.value.len()) {
            return false;
        }
        self.chosen_step.replace(step);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_critical_path_reported_again_alike_is_no_new_report() {
        let mut session = Session::default();
        let steps = || vec![CriticalStep::new(Some("bazel-out/a.o"), None)];
        assert!(session.report_critical_path(steps()));
        let traced = session.critical_path_revision();
        assert!(session.choose_step(Some(0)));
        assert!(!session.report_critical_path(steps()));
        assert_eq!(session.critical_path_revision(), traced);
        assert_eq!(session.chosen_step(), Some(0), "still a step of the path");
        assert!(session.report_critical_path(Vec::new()));
        assert_ne!(session.critical_path_revision(), traced);
    }

    #[test]
    fn a_critical_path_reported_anew_holds_no_step_chosen_of_the_last() {
        let mut session = Session::default();
        session.report_critical_path(vec![CriticalStep::new(Some("bazel-out/a.o"), None)]);
        let unchosen = session.chosen_step_revision();
        assert!(session.choose_step(Some(0)));
        assert_eq!(session.chosen_step(), Some(0));
        let chosen = session.chosen_step_revision();
        assert_ne!(chosen, unchosen);

        session.report_critical_path(vec![CriticalStep::new(Some("bazel-out/b.o"), None)]);
        assert_eq!(session.chosen_step(), None);
        assert_ne!(session.chosen_step_revision(), chosen);
    }

    #[test]
    fn a_step_past_the_critical_path_is_not_chosen() {
        let mut session = Session::default();
        session.report_critical_path(vec![CriticalStep::new(Some("bazel-out/a.o"), None)]);
        assert!(session.choose_step(Some(0)));
        let chosen = session.chosen_step_revision();
        assert!(!session.choose_step(Some(1)));
        assert_eq!(session.chosen_step(), Some(0));
        assert_eq!(session.chosen_step_revision(), chosen);
        assert!(session.choose_step(None));
        assert_eq!(session.chosen_step(), None);
    }

    #[test]
    fn millis_scale_to_micros_and_saturate() {
        assert_eq!(EpochMicros::from_millis(1_500).micros(), 1_500_000);
        assert_eq!(EpochMicros::from_millis(-2).micros(), -2_000);
        assert_eq!(EpochMicros::from_millis(i64::MAX).micros(), i64::MAX);
        assert_eq!(EpochMicros::from_millis(i64::MIN).micros(), i64::MIN);
    }

    #[test]
    fn seconds_round_to_the_nearest_micro_and_saturate() {
        assert_eq!(
            EpochMicros::from_secs_f64(1_781_439_245.231),
            EpochMicros::from_millis(1_781_439_245_231)
        );
        assert_eq!(EpochMicros::from_secs_f64(-0.000_001_6).micros(), -2);
        assert_eq!(EpochMicros::from_secs_f64(f64::INFINITY).micros(), i64::MAX);
        assert_eq!(EpochMicros::from_secs_f64(f64::NAN).micros(), 0);
    }

    #[test]
    fn an_offset_moves_an_instant_in_order_and_saturates() {
        let origin = EpochMicros::from_millis(1_781_439_245_231);
        let later = origin.plus_micros(1_500);
        assert_eq!(later.micros() - origin.micros(), 1_500);
        assert!(origin < later && origin.plus_micros(-1) < origin);
        assert_eq!(later.max(origin), later);
        assert_eq!(origin.plus_micros(i64::MAX).micros(), i64::MAX);
        assert_eq!(
            origin.plus_secs_f64(1.000_000_6),
            origin.plus_micros(1_000_001)
        );
        assert_eq!(origin.plus_secs_f64(f64::NAN), origin);
    }

    #[test]
    fn an_invocation_id_is_never_empty() {
        assert_eq!(InvocationId::new(""), None);
        let id = InvocationId::new("a5f7f3e6").expect("non-empty");
        assert_eq!(id.as_str(), "a5f7f3e6");
    }

    #[test]
    fn a_name_carries_the_checksum_only_to_tell_a_shared_mnemonic_apart() {
        let mut known = Configurations::default();
        known.insert("55817e2200e2".into(), "k8-fastbuild".into());
        assert_eq!(known.name("55817e2200e2").as_deref(), Some("k8-fastbuild"));

        known.insert("91e24984663c".into(), "k8-fastbuild".into());
        known.insert("0c3d9a7714be".into(), "k8-opt-exec".into());
        let name = |checksum| known.name(checksum);
        assert_eq!(name("55817e2200e2").as_deref(), Some("k8-fastbuild 55817e"));
        assert_eq!(name("91e24984663c").as_deref(), Some("k8-fastbuild 91e249"));
        assert_eq!(name("0c3d9a7714be").as_deref(), Some("k8-opt-exec"));
        assert_eq!(name("ffffffffffff"), None);
        assert_eq!(known.name_or_checksum("0c3d9a7714be"), "k8-opt-exec");
        assert_eq!(known.name_or_checksum("ffffffffffff"), "ffffff");
    }

    fn configurations(described: &[(&str, &str)]) -> Configurations {
        let mut configurations = Configurations::default();
        for (checksum, mnemonic) in described {
            configurations.insert((*checksum).into(), (*mnemonic).into());
        }
        configurations
    }

    #[test]
    fn a_session_knows_the_configurations_every_output_described() {
        let mut session = Session::default();
        assert_eq!(session.configurations().name("55817e2200e2"), None);

        session.add_configurations(configurations(&[("55817e2200e2", "k8-fastbuild")]));
        session.add_configurations(configurations(&[("0c3d9a7714be", "k8-opt-exec")]));
        let name = |checksum| session.configurations().name(checksum);
        assert_eq!(name("55817e2200e2").as_deref(), Some("k8-fastbuild"));
        assert_eq!(name("0c3d9a7714be").as_deref(), Some("k8-opt-exec"));
    }

    #[test]
    fn a_configuration_a_second_output_describes_keeps_its_first_description() {
        let mut session = Session::default();
        session.add_configurations(configurations(&[("55817e2200e2", "k8-fastbuild")]));
        session.add_configurations(configurations(&[
            ("55817e2200e2", "k8-opt"),
            ("91e24984663c", "k8-fastbuild"),
        ]));
        let name = |checksum| session.configurations().name(checksum);
        assert_eq!(name("55817e2200e2").as_deref(), Some("k8-fastbuild 55817e"));
        assert_eq!(name("91e24984663c").as_deref(), Some("k8-fastbuild 91e249"));
    }

    #[test]
    fn a_session_holds_the_outcomes_last_reported_to_it() {
        let app = ConfiguredTarget::new("//app".to_owned(), Some("55817e2200e2".to_owned()));
        let mut session = Session::default();
        assert!(session.outcomes().is_empty());

        let mut first = Outcomes::default();
        first.report_build(app.clone(), BuildOutcome::NotBuilt);
        session.report_outcomes(first);
        let mut second = Outcomes::default();
        second.report_build(app.clone(), BuildOutcome::Built);
        session.report_outcomes(second);

        assert_eq!(
            session.outcomes().build(app.borrowed()),
            Some(&BuildOutcome::Built)
        );
    }

    #[test]
    fn a_sessions_outcomes_are_revised_by_each_report_of_them_and_by_nothing_else() {
        let mut session = Session::default();
        let unreported = session.outcomes_revision();
        assert_eq!(unreported, Revision::default());

        session.add_configurations(configurations(&[("55817e2200e2", "k8-fastbuild")]));
        assert_eq!(session.outcomes_revision(), unreported);

        session.report_outcomes(Outcomes::default());
        let first = session.outcomes_revision();
        assert_ne!(first, unreported);
        session.add_configurations(configurations(&[("0c3d9a7714be", "k8-opt-exec")]));
        assert_eq!(session.outcomes_revision(), first);

        session.report_outcomes(Outcomes::default());
        assert_ne!(session.outcomes_revision(), first);
        assert_ne!(session.outcomes_revision(), unreported);
    }

    #[test]
    fn a_sessions_timings_are_revised_by_each_report_of_them_and_by_nothing_else() {
        let mut session = Session::default();
        let untimed = session.timings_revision();
        session.report_outcomes(Outcomes::default());
        assert_eq!(session.timings_revision(), untimed);

        let mut timings = Timings::default();
        let wall = std::time::Duration::from_millis(3);
        timings.record(Profiled {
            primary_output: Some("bazel-out/a.o"),
            owner: None,
            wall,
            spawns: Spawns::on(Runner::Local, wall),
        });
        let timing = Timing {
            wall,
            runner: Runner::Local,
        };
        session.report_timings(timings);
        let timed = session.timings_revision();
        assert_ne!(timed, untimed);
        assert_eq!(session.timings().action("bazel-out/a.o"), Some(timing));
        let outcomes = session.outcomes_revision();
        session.report_outcomes(Outcomes::default());
        assert_eq!(session.timings_revision(), timed);
        assert_ne!(session.outcomes_revision(), outcomes);

        session.report_timings(Timings::default());
        assert_ne!(session.timings_revision(), timed);
        assert!(session.timings().is_empty());
    }

    #[test]
    fn a_sessions_attempts_are_revised_by_each_report_of_them_and_by_nothing_else() {
        let mut session = Session::default();
        let unattempted = session.attempts_revision();
        session.report_timings(Timings::default());
        assert_eq!(session.attempts_revision(), unattempted);

        let attempt = attempts::tests::attempt(0, 10, &[]);
        let mut attempts = Attempts::default();
        attempts.record(Some("bazel-out/a.o"), None, attempt);
        session.report_attempts(attempts);
        let attempted = session.attempts_revision();
        assert_ne!(attempted, unattempted);
        assert_eq!(
            session.attempts().action("bazel-out/a.o"),
            Some(&[attempt][..])
        );
        session.report_timings(Timings::default());
        assert_eq!(session.attempts_revision(), attempted);

        session.report_attempts(Attempts::default());
        assert_ne!(session.attempts_revision(), attempted);
        assert!(session.attempts().is_empty());
    }

    #[test]
    fn a_sessions_logged_spawns_are_revised_by_each_report_of_them_and_by_nothing_else() {
        let mut session = Session::default();
        let unlogged = session.logged_spawns_revision();
        assert!(session.logged_spawns().is_none());
        session.report_timings(Timings::default());
        assert_eq!(session.logged_spawns_revision(), unlogged);

        session.report_logged_spawns(LoggedSpawns::default());
        let logged = session.logged_spawns_revision();
        assert_ne!(logged, unlogged);
        assert!(
            session
                .logged_spawns()
                .is_some_and(|spawns| spawns.is_empty())
        );
        session.report_outcomes(Outcomes::default());
        assert_eq!(session.logged_spawns_revision(), logged);

        session.report_logged_spawns(LoggedSpawns::default());
        assert_ne!(session.logged_spawns_revision(), logged);
    }

    #[test]
    fn a_sessions_action_graph_is_revised_by_each_report_of_it_and_by_nothing_else() {
        let mut session = Session::default();
        let ungraphed = session.action_graph_revision();
        assert!(session.action_graph().is_none());
        session.report_logged_spawns(LoggedSpawns::default());
        assert_eq!(session.action_graph_revision(), ungraphed);

        session.report_action_graph(ActionGraph::new(
            vec![Some("bazel-out/a.o".to_owned())],
            Vec::new(),
        ));
        let graphed = session.action_graph_revision();
        assert_ne!(graphed, ungraphed);
        assert_eq!(session.action_graph().map(|graph| graph.len()), Some(1));
        session.report_timings(Timings::default());
        assert_eq!(session.action_graph_revision(), graphed);

        session.report_action_graph(ActionGraph::default());
        assert_ne!(session.action_graph_revision(), graphed);
        assert!(session.action_graph().is_some_and(|graph| graph.is_empty()));
    }

    #[test]
    fn a_checksum_described_again_keeps_its_first_description() {
        let mut known = Configurations::default();
        known.insert("55817e2200e2".into(), "k8-fastbuild".into());
        known.insert("55817e2200e2".into(), "k8-opt".into());
        assert_eq!(known.name("55817e2200e2").as_deref(), Some("k8-fastbuild"));
    }
}
