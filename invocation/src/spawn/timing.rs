use std::time::Duration;

/// A stage of a spawn's run that Bazel's spawn metrics time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Converting the spawn into a remote execution request.
    Parse,
    Upload,
    Network,
    Queue,
    Setup,
    /// Running the subprocess: Bazel's execution wall time.
    Execute,
    ProcessOutputs,
    Fetch,
    /// Previous failed attempts.
    Retry,
    /// What remains of a spawn's total once every other phase is accounted.
    Other,
}

impl Phase {
    /// Every phase, in the order a reader is shown them.
    pub const ALL: [Phase; 10] = [
        Phase::Parse,
        Phase::Upload,
        Phase::Network,
        Phase::Queue,
        Phase::Setup,
        Phase::Execute,
        Phase::ProcessOutputs,
        Phase::Fetch,
        Phase::Retry,
        Phase::Other,
    ];

    /// The phase's place in [`ALL`](Self::ALL).
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// The phases a spawn reports itself, all of [`Phase::ALL`] but [`Phase::Other`].
pub const REPORTED_PHASES: usize = Phase::ALL.len() - 1;

/// Spawn time by phase, for one spawn or summed over several.
///
/// [`Phase::Other`] holds each spawn's total less its reported phases, or
/// nothing where those phases overlap to exceed the total, so the phases sum
/// to at least the total.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PhaseTiming {
    total: Duration,
    phases: [Duration; Phase::ALL.len()],
}

impl PhaseTiming {
    /// A spawn's timing from its total and its reported phases, in the order of
    /// [`Phase::ALL`].
    pub fn new(total: Duration, reported: [Duration; REPORTED_PHASES]) -> PhaseTiming {
        let accounted = reported
            .iter()
            .fold(Duration::ZERO, |sum, &phase| sum.saturating_add(phase));
        let mut phases = [Duration::ZERO; Phase::ALL.len()];
        phases[..REPORTED_PHASES].copy_from_slice(&reported);
        phases[Phase::Other.index()] = total.saturating_sub(accounted);
        PhaseTiming { total, phases }
    }

    /// The spawn's total time, as Bazel measured it.
    pub fn total(&self) -> Duration {
        self.total
    }

    pub fn phase(&self, phase: Phase) -> Duration {
        self.phases[phase.index()]
    }

    /// Whether execution is the only phase reported: a runner that measures no
    /// other phase, as the local sandboxes do, rather than one whose other
    /// phases took no time.
    pub fn reports_execution_only(&self) -> bool {
        self.phase(Phase::Execute) > Duration::ZERO
            && Phase::ALL
                .into_iter()
                .filter(|&phase| !matches!(phase, Phase::Execute | Phase::Other))
                .all(|phase| self.phase(phase).is_zero())
    }

    pub fn add(&mut self, other: &PhaseTiming) {
        self.total = self.total.saturating_add(other.total);
        for (sum, phase) in self.phases.iter_mut().zip(other.phases) {
            *sum = sum.saturating_add(phase);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    fn reported(set: &[(Phase, u64)]) -> [Duration; REPORTED_PHASES] {
        let mut phases = [Duration::ZERO; REPORTED_PHASES];
        for &(phase, millis) in set {
            phases[phase.index()] = ms(millis);
        }
        phases
    }

    #[test]
    fn all_lists_every_phase_at_its_index() {
        for (i, phase) in Phase::ALL.into_iter().enumerate() {
            assert_eq!(phase.index(), i);
        }
    }

    #[test]
    fn other_is_the_unaccounted_remainder() {
        let timing = PhaseTiming::new(
            ms(100),
            reported(&[(Phase::Network, 30), (Phase::Execute, 50)]),
        );
        assert_eq!(timing.phase(Phase::Other), ms(20));
        assert_eq!(timing.total(), ms(100));
    }

    #[test]
    fn overlapping_phases_leave_no_other() {
        let timing = PhaseTiming::new(
            ms(100),
            reported(&[(Phase::Network, 80), (Phase::Fetch, 40)]),
        );
        assert_eq!(timing.phase(Phase::Other), Duration::ZERO);
    }

    #[test]
    fn execution_only_ignores_the_remainder() {
        let sandboxed = PhaseTiming::new(ms(100), reported(&[(Phase::Execute, 90)]));
        assert!(sandboxed.reports_execution_only());
        let remote = PhaseTiming::new(
            ms(100),
            reported(&[(Phase::Execute, 90), (Phase::Queue, 1)]),
        );
        assert!(!remote.reports_execution_only());
        let unmeasured = PhaseTiming::new(ms(100), reported(&[]));
        assert!(!unmeasured.reports_execution_only());
    }

    #[test]
    fn add_sums_totals_and_phases() {
        let mut sum = PhaseTiming::new(ms(10), reported(&[(Phase::Execute, 4)]));
        sum.add(&PhaseTiming::new(ms(5), reported(&[(Phase::Execute, 5)])));
        assert_eq!(sum.total(), ms(15));
        assert_eq!(sum.phase(Phase::Execute), ms(9));
        assert_eq!(sum.phase(Phase::Other), ms(6));
    }

    #[test]
    fn corrupt_durations_saturate() {
        let huge = [Duration::MAX; REPORTED_PHASES];
        let mut sum = PhaseTiming::new(Duration::MAX, huge);
        assert_eq!(sum.phase(Phase::Other), Duration::ZERO);
        sum.add(&PhaseTiming::new(Duration::MAX, huge));
        assert_eq!(sum.total(), Duration::MAX);
        assert_eq!(sum.phase(Phase::Network), Duration::MAX);
    }
}
