//! The vocabulary shared by the readers of one Bazel invocation's outputs:
//! each output states the same facts in its own encoding, and what the readers
//! return must be comparable to be joined.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

mod analysed;
mod attempts;
mod merged;
mod outcomes;
mod runner;
pub mod spawn;
mod timings;

pub use analysed::Analysed;
pub use attempts::{
    Attempt, Attempts, Cache, DescribedAction, Done, HeldAction, Phase, SpawnCache, State,
    TEST_LOG, Venue,
};
pub use merged::{ActionAttempts, AttemptSource, MergedAttempts, Resolved};
pub use outcomes::{
    BuildOutcome, Cause, ConfiguredTarget, DatedOutcomes, FailedAction, Folded, Known, Outcomes,
    Report, TestOutcome, TestStatus,
};
pub use runner::{Runner, Spawns};
pub use timings::{Profiled, Share, Spent, Timing, Timings};

use spawn::LoggedSpawns;

const MICROS_PER_MILLI: i64 = 1_000;
const MICROS_PER_WHOLE_SECOND: i64 = 1_000_000;
const MICROS_PER_SECOND: f64 = MICROS_PER_WHOLE_SECOND as f64;
const NANOS_PER_MICRO: i32 = 1_000;

/// A wall-clock instant, in microseconds since the Unix epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EpochMicros(i64);

impl EpochMicros {
    /// The instant `millis` milliseconds after the Unix epoch, saturating at
    /// the representable range.
    pub const fn from_millis(millis: i64) -> EpochMicros {
        EpochMicros(millis.saturating_mul(MICROS_PER_MILLI))
    }

    pub const fn from_micros(micros: i64) -> EpochMicros {
        EpochMicros(micros)
    }

    /// The instant `seconds` and `nanos` nanoseconds after the Unix epoch, as
    /// a protobuf `Timestamp` gives one, to the microsecond below and
    /// saturating at the representable range.
    pub const fn from_timestamp(seconds: i64, nanos: i32) -> EpochMicros {
        EpochMicros(
            seconds
                .saturating_mul(MICROS_PER_WHOLE_SECOND)
                .saturating_add((nanos / NANOS_PER_MICRO) as i64),
        )
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

    /// The instant `duration` after this one, to the microsecond below and
    /// saturating at the representable range.
    pub fn after(self, duration: Duration) -> EpochMicros {
        self.plus_micros(whole_micros(duration))
    }

    /// The instant `duration` before this one, to the microsecond above and
    /// saturating at the representable range.
    pub fn before(self, duration: Duration) -> EpochMicros {
        EpochMicros(self.0.saturating_sub(whole_micros(duration)))
    }
}

fn whole_micros(duration: Duration) -> i64 {
    i64::try_from(duration.as_micros()).unwrap_or(i64::MAX)
}

/// A stretch of wall-clock time, starting no later than it ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval {
    start: EpochMicros,
    end: EpochMicros,
}

impl Interval {
    /// The stretch from `start` to `end`; `None` where `end` comes first.
    pub fn new(start: EpochMicros, end: EpochMicros) -> Option<Interval> {
        (start <= end).then_some(Interval { start, end })
    }

    /// The stretch of no time at `at`.
    pub fn at(at: EpochMicros) -> Interval {
        Interval { start: at, end: at }
    }

    pub fn start(self) -> EpochMicros {
        self.start
    }

    pub fn end(self) -> EpochMicros {
        self.end
    }

    /// The least interval holding both.
    pub fn hull(self, other: Interval) -> Interval {
        Interval {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
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

/// What a build reported, where the reports are dated as they became known.
pub enum HeldOutcomes {
    Undated(Outcomes),
    Dated(DatedOutcomes),
}

impl Default for HeldOutcomes {
    fn default() -> Self {
        HeldOutcomes::Undated(Outcomes::default())
    }
}

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
        self.revise(|held| *held = value);
    }

    fn revise(&mut self, revise: impl FnOnce(&mut T)) {
        revise(&mut self.value);
        self.revision.0 += 1;
    }
}

/// What the views of one invocation's outputs share. Each output's reader adds
/// what it learns of the invocation, for the views of the other outputs to read.
#[derive(Default)]
pub struct Session {
    /// By [`AttemptSource::index`]: each source reports attempts of its own, which
    /// stand in place only of those it reported before; `None` until it reports.
    attempts: Reported<[Option<Attempts>; AttemptSource::ALL.len()]>,
    configurations: Configurations,
    outcomes: Reported<HeldOutcomes>,
    analysed: Reported<Analysed>,
    timings: Reported<Timings>,
    /// Shared, as a derivation off the UI thread reads it. Held, every
    /// spawn's outputs with it, for as long as the session is, whether or
    /// not a view of the invocation reads them.
    logged_spawns: Reported<Option<Arc<LoggedSpawns>>>,
}

impl Session {
    pub fn configurations(&self) -> &Configurations {
        &self.configurations
    }

    /// Adds the configurations one of the invocation's outputs describes.
    pub fn add_configurations(&mut self, described: Configurations) {
        self.configurations.extend(described);
    }

    /// What the invocation's build reported, at whatever instant.
    pub fn outcomes(&self) -> &Outcomes {
        match &self.outcomes.value {
            HeldOutcomes::Undated(outcomes) => outcomes,
            HeldOutcomes::Dated(dated) => dated.outcomes(),
        }
    }

    /// The same, each report dated as it became known; `None` where the
    /// build's reports are not dated.
    pub fn dated_outcomes(&self) -> Option<&DatedOutcomes> {
        match &self.outcomes.value {
            HeldOutcomes::Undated(_) => None,
            HeldOutcomes::Dated(dated) => Some(dated),
        }
    }

    /// The same for as long as [`outcomes`](Self::outcomes) and
    /// [`dated_outcomes`](Self::dated_outcomes) are those of one report.
    pub fn outcomes_revision(&self) -> Revision {
        self.outcomes.revision
    }

    pub fn report_outcomes(&mut self, reported: Outcomes) {
        self.outcomes.replace(HeldOutcomes::Undated(reported));
    }

    pub fn report_dated_outcomes(&mut self, reported: DatedOutcomes) {
        self.outcomes.replace(HeldOutcomes::Dated(reported));
    }

    /// When the invocation's build finished analysing each top-level target
    /// it was asked for.
    pub fn analysed(&self) -> &Analysed {
        &self.analysed.value
    }

    pub fn analysed_revision(&self) -> Revision {
        self.analysed.revision
    }

    pub fn report_analysed(&mut self, reported: Analysed) {
        self.analysed.replace(reported);
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

    /// When the invocation's actions were executed and how each ended, as
    /// every source reporting them tells together.
    pub fn attempts(&self) -> MergedAttempts<'_> {
        MergedAttempts::new(
            AttemptSource::ALL
                .into_iter()
                .zip(&self.attempts.value)
                .filter_map(|(source, reported)| Some((source, reported.as_ref()?))),
        )
    }

    /// The same for as long as no source reports attempts anew.
    pub fn attempts_revision(&self) -> Revision {
        self.attempts.revision
    }

    /// Takes `reported` for the attempts `source` tells of.
    pub fn report_attempts(&mut self, source: AttemptSource, reported: Attempts) {
        self.attempts
            .revise(|by_source| by_source[source.index()] = Some(reported));
    }

    /// The spawns the invocation's execution log records, `None` until one
    /// is reported.
    pub fn logged_spawns(&self) -> Option<&Arc<LoggedSpawns>> {
        self.logged_spawns.value.as_ref()
    }

    /// The same for as long as [`logged_spawns`](Self::logged_spawns) are
    /// those of one report, whatever else the session learns meanwhile.
    pub fn logged_spawns_revision(&self) -> Revision {
        self.logged_spawns.revision
    }

    /// Takes `reported` for the spawns the invocation's execution log records.
    /// An invocation writes one execution log, so these stand in place of any
    /// reported before.
    pub fn report_logged_spawns(&mut self, reported: LoggedSpawns) {
        self.logged_spawns.replace(Some(Arc::new(reported)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn millis_scale_to_micros_and_saturate() {
        assert_eq!(EpochMicros::from_millis(1_500).micros(), 1_500_000);
        assert_eq!(EpochMicros::from_millis(-2).micros(), -2_000);
        assert_eq!(EpochMicros::from_millis(i64::MAX).micros(), i64::MAX);
        assert_eq!(EpochMicros::from_millis(i64::MIN).micros(), i64::MIN);
    }

    #[test]
    fn an_interval_starts_no_later_than_it_ends() {
        let at = |millis: i64| EpochMicros::from_millis(1_781_439_245_000 + millis);
        assert_eq!(Interval::new(at(2), at(1)), None);
        let instant = Interval::at(at(1));
        assert_eq!(Interval::new(at(1), at(1)), Some(instant));
        assert_eq!((instant.start(), instant.end()), (at(1), at(1)));
        let later = Interval::new(at(5), at(8)).expect("ordered");
        assert_eq!(instant.hull(later), Interval::new(at(1), at(8)).unwrap());
    }

    #[test]
    fn a_duration_moves_an_instant_by_whole_microseconds() {
        let at = EpochMicros::from_micros(1_000);
        let lasted = Duration::from_nanos(2_500);
        assert_eq!(at.after(lasted), EpochMicros::from_micros(1_002));
        assert_eq!(at.before(lasted), EpochMicros::from_micros(998));
        assert_eq!(EpochMicros::from_timestamp(1, 2_999).micros(), 1_000_002);
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
    fn a_session_holds_the_dated_outcomes_last_reported_to_it() {
        let app = ConfiguredTarget::new("//app".to_owned(), Some("55817e2200e2".to_owned()));
        let at = EpochMicros::from_millis(1_781_439_245_231);
        let mut session = Session::default();
        assert!(session.dated_outcomes().is_none());

        let reports = [(at, Report::Build(app.clone(), BuildOutcome::Built))];
        session.report_dated_outcomes(DatedOutcomes::new(reports));
        let dated = session.outcomes_revision();
        assert_eq!(
            session.outcomes().build(app.borrowed()),
            Some(&BuildOutcome::Built)
        );
        let held = session.dated_outcomes().expect("dated");
        assert_eq!(held.known_at(at), held.all_known());

        session.report_outcomes(Outcomes::default());
        assert_ne!(session.outcomes_revision(), dated);
        assert!(session.dated_outcomes().is_none());
        assert!(session.outcomes().is_empty());
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
    fn a_sessions_analysis_is_revised_by_each_report_of_it_and_by_nothing_else() {
        let app = ConfiguredTarget::new("//app".to_owned(), Some("55817e2200e2".to_owned()));
        let at = EpochMicros::from_millis(1_781_439_245_231);
        let mut session = Session::default();
        let unanalysed = session.analysed_revision();
        session.report_outcomes(Outcomes::default());
        assert_eq!(session.analysed_revision(), unanalysed);

        let mut analysed = Analysed::default();
        analysed.record(app.clone(), at);
        session.report_analysed(analysed);
        assert_ne!(session.analysed_revision(), unanalysed);
        assert_eq!(session.analysed().at(app.borrowed()), Some(at));
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
        session.report_attempts(AttemptSource::Profile, attempts);
        let attempted = session.attempts_revision();
        assert_ne!(attempted, unattempted);
        let held = |session: &Session| {
            session
                .attempts()
                .action("bazel-out/a.o")
                .map(|tried| tried.iter().collect::<Vec<_>>())
        };
        assert_eq!(held(&session), Some(vec![attempt]));
        session.report_timings(Timings::default());
        assert_eq!(session.attempts_revision(), attempted);

        session.report_attempts(AttemptSource::ExecutionLog, Attempts::default());
        let logged = session.attempts_revision();
        assert_ne!(logged, attempted);
        assert_eq!(
            held(&session),
            Some(vec![attempt]),
            "another source's report"
        );

        session.report_attempts(AttemptSource::Profile, Attempts::default());
        assert_ne!(session.attempts_revision(), logged);
        assert!(session.attempts().is_empty());
    }

    #[test]
    fn a_session_merges_the_attempts_of_every_source() {
        let mut session = Session::default();
        let profiled = attempts::tests::attempt(0, 10, &[]);
        let mut profile = Attempts::default();
        profile.record(Some("bazel-out/a.o"), None, profiled);
        session.report_attempts(AttemptSource::Profile, profile);
        let mut log = Attempts::default();
        let failed = Attempt::ended(profiled.start(), profiled.end(), [], Done::Failed);
        log.record(Some("bazel-out/a.o"), None, failed);
        log.record(Some("bazel-out/b.o"), None, failed);
        session.report_attempts(AttemptSource::ExecutionLog, log);

        let merged = session.attempts();
        let a = merged.action("bazel-out/a.o").expect("held");
        assert_eq!(a.source(), AttemptSource::Profile);
        assert_eq!(
            a.iter().map(|tried| tried.done()).collect::<Vec<_>>(),
            [Done::Failed]
        );
        let b = merged.action("bazel-out/b.o").expect("held");
        assert_eq!(b.source(), AttemptSource::ExecutionLog);
    }

    #[test]
    fn a_session_corroborates_an_ending_only_by_a_source_that_reported() {
        let mut session = Session::default();
        let mut log = Attempts::default();
        let spawnless = Attempt::ended(
            EpochMicros::from_millis(0),
            EpochMicros::from_millis(10),
            [],
            Done::Spawnless,
        );
        log.record(Some("bazel-out/a.o"), None, spawnless);
        session.report_attempts(AttemptSource::ExecutionGraphLog, log);
        let ending = |session: &Session| {
            let merged = session.attempts();
            let held = merged.action("bazel-out/a.o").expect("held");
            held.iter().map(|tried| tried.done()).collect::<Vec<_>>()
        };
        assert_eq!(ending(&session), [Done::Spawnless]);

        let mut profile = Attempts::default();
        profile.mark_exhaustive();
        session.report_attempts(AttemptSource::Profile, profile);
        assert_eq!(ending(&session), [Done::Cached(Cache::Action)]);
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
    fn a_checksum_described_again_keeps_its_first_description() {
        let mut known = Configurations::default();
        known.insert("55817e2200e2".into(), "k8-fastbuild".into());
        known.insert("55817e2200e2".into(), "k8-opt".into());
        assert_eq!(known.name("55817e2200e2").as_deref(), Some("k8-fastbuild"));
    }
}
