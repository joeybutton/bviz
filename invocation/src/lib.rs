//! The vocabulary shared by the readers of one Bazel invocation's outputs:
//! each output states the same facts in its own encoding, and what the readers
//! return must be comparable to be joined.

use std::collections::HashMap;
use std::sync::Arc;

mod outcomes;

pub use outcomes::{
    BuildOutcome, Cause, ConfiguredTarget, FailedAction, Outcomes, TestOutcome, TestStatus,
};

const MICROS_PER_MILLI: i64 = 1_000;
const MICROS_PER_SECOND: f64 = 1e6;

/// A wall-clock instant, in microseconds since the Unix epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpochMicros(i64);

impl EpochMicros {
    /// The instant `millis` milliseconds after the Unix epoch, saturating at
    /// the representable range.
    pub const fn from_millis(millis: i64) -> EpochMicros {
        EpochMicros(millis.saturating_mul(MICROS_PER_MILLI))
    }

    /// The instant `seconds` seconds after the Unix epoch, to the nearest
    /// microsecond and saturating at the representable range; the epoch itself
    /// for a NaN.
    pub fn from_secs_f64(seconds: f64) -> EpochMicros {
        EpochMicros((seconds * MICROS_PER_SECOND).round() as i64)
    }

    pub const fn micros(self) -> i64 {
        self.0
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

/// Leading characters of a configuration checksum that [`Configurations::name`]
/// tells apart configurations sharing a mnemonic by.
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
            true => {
                let short: String = checksum.chars().take(CHECKSUM_SHOWN).collect();
                format!("{mnemonic} {short}")
            }
            false => mnemonic.clone(),
        })
    }
}

/// Which of the reports of outcomes a [`Session`] has taken its outcomes are
/// of. A reader that keeps the one its reading was taken at can tell whether
/// the outcomes are still those it read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Revision(u64);

/// What the views of one invocation's outputs share. Each output's reader adds
/// what it learns of the invocation, for the views of the other outputs to read.
#[derive(Default)]
pub struct Session {
    configurations: Configurations,
    outcomes: Outcomes,
    reported: Revision,
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
        &self.outcomes
    }

    /// The same for as long as [`outcomes`](Self::outcomes) are those of one
    /// report, whatever else the session learns meanwhile.
    pub fn outcomes_revision(&self) -> Revision {
        self.reported
    }

    /// Takes `reported` for what the invocation's build reported. A build
    /// writes one build event stream, so these stand in place of any reported
    /// before.
    pub fn report_outcomes(&mut self, reported: Outcomes) {
        self.outcomes = reported;
        self.reported.0 += 1;
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
    fn a_checksum_described_again_keeps_its_first_description() {
        let mut known = Configurations::default();
        known.insert("55817e2200e2".into(), "k8-fastbuild".into());
        known.insert("55817e2200e2".into(), "k8-opt".into());
        assert_eq!(known.name("55817e2200e2").as_deref(), Some("k8-fastbuild"));
    }
}
