//! When each execution attempt of a build's actions entered each phase.

use std::collections::HashMap;

use crate::outcomes::PerTarget;
use crate::{ConfiguredTarget, EpochMicros, Interval};

/// Where an attempt's spawn runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Venue {
    Local,
    Remote,
}

impl Venue {
    pub const ALL: [Venue; 2] = [Venue::Local, Venue::Remote];
}

/// How far an execution attempt has come, in the order Bazel's UI advances an
/// action through (`UiStateTracker.ActionPhase`):
/// <https://github.com/bazelbuild/bazel/blob/9.1.0/src/main/java/com/google/devtools/build/lib/runtime/UiStateTracker.java#L196-L214>
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Phase {
    /// Discovering, fetching, or staging inputs, or setting up a sandbox or
    /// worker.
    Preparing,
    /// Looking the action up in a disk or remote cache.
    CheckingCache,
    /// Waiting for local resources or in a remote executor's queue.
    Scheduling,
    /// Running on either venue, which an attempt may leave for the other, as
    /// a local fallback from a failed remote execution does, without moving
    /// back.
    Running(Venue),
}

impl Phase {
    /// Every phase, at its [`index`](Self::index).
    pub const ALL: [Phase; 5] = [
        Phase::Preparing,
        Phase::CheckingCache,
        Phase::Scheduling,
        Phase::Running(Venue::Local),
        Phase::Running(Venue::Remote),
    ];

    pub const fn index(self) -> usize {
        match self {
            Phase::Preparing => 0,
            Phase::CheckingCache => 1,
            Phase::Scheduling => 2,
            Phase::Running(Venue::Local) => 3,
            Phase::Running(Venue::Remote) => 4,
        }
    }

    /// The phase's slot in an [`Attempt`]'s table of when it entered each;
    /// `None` for `Preparing`, which an attempt enters at its start.
    const fn past_preparing(self) -> Option<usize> {
        self.index().checked_sub(1)
    }

    /// How far the phase is along, the venues of running alike.
    const fn progress(self) -> usize {
        match self {
            Phase::Running(_) => Phase::Running(Venue::Local).index(),
            phase => phase.index(),
        }
    }
}

/// How an attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Done {
    /// It ran, or ended without being answered by a cache, and did not fail.
    Executed,
    /// A cache answered it: it never ran.
    Cached(Cache),
    Failed,
}

impl Done {
    /// How an action ended, told by two sources that each saw it end as one
    /// of the two says: as the one [telling](Self::telling) more says,
    /// `self` where they tell as much.
    pub(crate) fn merge(self, other: Done) -> Done {
        match other.telling() > self.telling() {
            true => other,
            false => self,
        }
    }

    /// How much an ending tells of how the action came to its outputs, so
    /// that a source telling more outweighs one telling less. A failure
    /// outweighs every other ending, as any source seeing it saw the action
    /// fail; a cache named outweighs one left unnamed, which outweighs an
    /// execution, as a source seeing an attempt end without seeing the cache
    /// answer it reads it executed. Up to date outweighs nothing: only an
    /// action the action cache did not find up to date runs a spawn or ends
    /// within Bazel, so a source seeing it do either refutes it.
    const fn telling(self) -> u8 {
        match self {
            Done::Cached(Cache::Action) => 0,
            Done::Executed => 1,
            Done::Cached(Cache::Spawn(SpawnCache::RemoteOrDisk)) => 2,
            Done::Cached(Cache::Spawn(SpawnCache::Remote | SpawnCache::Disk)) => 3,
            Done::Failed => 4,
        }
    }
}

/// The cache that answered an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cache {
    /// Bazel's action cache found the action's outputs up to date, so the
    /// action ran no spawn at all.
    Action,
    /// A cache answered the action's spawn.
    Spawn(SpawnCache),
}

/// A cache that answers a spawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpawnCache {
    Remote,
    Disk,
    /// A remote or a disk cache, where what saw the hit cannot tell which.
    RemoteOrDisk,
}

impl SpawnCache {
    /// The cache a spawn answered by a cache was answered by, as the name of
    /// the runner Bazel gives the spawn's result tells:
    /// <https://github.com/bazelbuild/bazel/blob/9.2.0/src/main/java/com/google/devtools/build/lib/remote/util/Utils.java#L159>
    pub fn answering(runner: &str) -> SpawnCache {
        match runner {
            "remote cache hit" => SpawnCache::Remote,
            "disk cache hit" => SpawnCache::Disk,
            _ => SpawnCache::RemoteOrDisk,
        }
    }
}

/// The name of a test's log, the primary output of the test's action, in the
/// directory the test's other outputs are written to.
pub const TEST_LOG: &str = "test.log";

/// Where an attempt is at one instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    /// Not yet started.
    Pending,
    In(Phase),
    Done(Done),
}

/// One execution of an action, from its start until it ended. An action Bazel
/// executes again, as it does a rewound one, has an attempt for each
/// execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attempt {
    start: EpochMicros,
    end: EpochMicros,
    /// By [`Phase::past_preparing`], when the attempt entered each phase it
    /// entered.
    entered: [Option<EpochMicros>; Phase::ALL.len() - 1],
    done: Done,
}

impl Attempt {
    /// The attempt lasting from `start` until `end` that showed each phase of
    /// `seen` from the instant paired with it, its ending told by the phases
    /// it entered: one that checked a cache and never ran was answered by a
    /// remote or disk cache, and any other executed.
    pub fn new(
        start: EpochMicros,
        end: EpochMicros,
        seen: impl IntoIterator<Item = (Phase, EpochMicros)>,
    ) -> Attempt {
        let mut attempt = Attempt::ended(start, end, seen, Done::Executed);
        let ran = Venue::ALL
            .into_iter()
            .any(|venue| attempt.entered(Phase::Running(venue)).is_some());
        if !ran && attempt.entered(Phase::CheckingCache).is_some() {
            attempt.done = Done::Cached(Cache::Spawn(SpawnCache::RemoteOrDisk));
        }
        attempt
    }

    /// The attempt lasting from `start` until `end` that showed each phase of
    /// `seen` from the instant paired with it and ended as `done` says. A
    /// phase only moves forward, so one seen after a further one was entered
    /// is not entered; nor is one seen again, or outside the attempt.
    pub fn ended(
        start: EpochMicros,
        end: EpochMicros,
        seen: impl IntoIterator<Item = (Phase, EpochMicros)>,
        done: Done,
    ) -> Attempt {
        let end = end.max(start);
        let mut first = [None; Phase::ALL.len()];
        for (phase, at) in seen {
            if (start..=end).contains(&at) {
                let earliest = &mut first[phase.index()];
                *earliest = Some(earliest.map_or(at, |earlier: EpochMicros| earlier.min(at)));
            }
        }
        let overtaken = |phase: Phase, at: EpochMicros| {
            Phase::ALL.into_iter().any(|further| {
                further.progress() > phase.progress()
                    && first[further.index()].is_some_and(|then| then < at)
            })
        };
        let mut entered = [None; Phase::ALL.len() - 1];
        for phase in Phase::ALL {
            if let Some(slot) = phase.past_preparing() {
                entered[slot] = first[phase.index()].filter(|&at| !overtaken(phase, at));
            }
        }
        Attempt {
            start,
            end,
            entered,
            done,
        }
    }

    pub fn start(&self) -> EpochMicros {
        self.start
    }

    pub fn end(&self) -> EpochMicros {
        self.end
    }

    /// From the attempt's start to its end.
    pub fn span(&self) -> Interval {
        Interval {
            start: self.start,
            end: self.end,
        }
    }

    /// When the attempt entered `phase`; `None` for one it never entered.
    pub fn entered(&self, phase: Phase) -> Option<EpochMicros> {
        match phase.past_preparing() {
            None => Some(self.start),
            Some(slot) => self.entered[slot],
        }
    }

    /// How the attempt ended.
    pub fn done(&self) -> Done {
        self.done
    }

    /// The attempt, ended as `done` says.
    pub(crate) fn ending(self, done: Done) -> Attempt {
        Attempt { done, ..self }
    }

    /// The attempt carried on by `later`, a step of the same execution: it
    /// lasts until `later` ends, and fails where `later` fails.
    fn continued(self, later: Attempt) -> Attempt {
        Attempt {
            end: self.end.max(later.end),
            done: match later.done {
                Done::Failed => Done::Failed,
                _ => self.done,
            },
            ..self
        }
    }

    /// Where the attempt is at `at`: it starts at its start, and has ended at
    /// its end.
    pub fn state_at(&self, at: EpochMicros) -> State {
        if at < self.start {
            return State::Pending;
        }
        if at >= self.end {
            return State::Done(self.done());
        }
        let latest = Phase::ALL
            .into_iter()
            .filter_map(|phase| Some((self.entered(phase)?, phase)))
            .filter(|&(entered, _)| entered <= at)
            .max_by_key(|&(entered, phase)| (entered, phase.progress()));
        State::In(latest.map_or(Phase::Preparing, |(_, phase)| phase))
    }
}

/// The attempts of one action known by its primary output.
struct Attempted {
    owner: Option<ConfiguredTarget>,
    attempts: Vec<Attempt>,
}

/// An action known by its primary output: that output's exec path, the
/// action's owner where named, and its attempts.
pub type HeldAction<'a> = (&'a str, Option<ConfiguredTarget<&'a str>>, &'a [Attempt]);

fn held_action<'a>(primary_output: &'a str, attempted: &'a Attempted) -> HeldAction<'a> {
    (
        primary_output,
        attempted.owner.as_ref().map(ConfiguredTarget::borrowed),
        attempted.attempts.as_slice(),
    )
}

/// The execution attempts of a build's actions. An action is known by its
/// primary output where its profile names one, kept with its owner where the
/// profile names that too; the attempts of actions named only by their owner
/// are pooled under it; an attempt named by neither is not kept.
#[derive(Default)]
pub struct Attempts {
    actions: HashMap<String, Attempted>,
    by_owner: PerTarget<Vec<Attempt>>,
}

impl Attempts {
    pub fn with_capacity(actions: usize) -> Self {
        Self {
            actions: HashMap::with_capacity(actions),
            ..Self::default()
        }
    }

    /// Records `attempt` of the action `primary_output` names, or failing
    /// that `owner`, after those recorded of it before.
    pub fn record(
        &mut self,
        primary_output: Option<&str>,
        owner: Option<ConfiguredTarget<&str>>,
        attempt: Attempt,
    ) {
        match (primary_output, owner) {
            (Some(primary_output), owner) => {
                self.actions
                    .entry(primary_output.to_owned())
                    .or_insert_with(|| Attempted {
                        owner: owner.map(ConfiguredTarget::owned),
                        attempts: Vec::with_capacity(1),
                    })
                    .attempts
                    .push(attempt);
            }
            (None, Some(owner)) => {
                self.by_owner
                    .report(owner.owned(), vec![attempt], |attempts, more| {
                        attempts.extend(more)
                    });
            }
            (None, None) => {}
        }
    }

    /// Records `later`, a step of the attempt recorded last of the action
    /// `primary_output` names, as [carrying on](Attempt::continued) that
    /// attempt; where none is recorded, as [`record`](Self::record) does.
    pub(crate) fn record_continued(
        &mut self,
        primary_output: Option<&str>,
        owner: Option<ConfiguredTarget<&str>>,
        later: Attempt,
    ) {
        let last = primary_output
            .and_then(|primary_output| self.actions.get_mut(primary_output))
            .and_then(|attempted| attempted.attempts.last_mut());
        match last {
            Some(last) => *last = last.continued(later),
            None => self.record(primary_output, owner, later),
        }
    }

    /// Whether no attempt is recorded.
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty() && self.by_owner.is_empty()
    }

    /// The attempts of the action whose primary output has the exec path
    /// `primary_output`, in the order recorded.
    pub fn action(&self, primary_output: &str) -> Option<&[Attempt]> {
        self.actions
            .get(primary_output)
            .map(|attempted| attempted.attempts.as_slice())
    }

    /// The action whose primary output has the exec path `primary_output`,
    /// as [`actions`](Self::actions) yields it.
    pub(crate) fn attempted(&self, primary_output: &str) -> Option<HeldAction<'_>> {
        let (primary_output, attempted) = self.actions.get_key_value(primary_output)?;
        Some(held_action(primary_output, attempted))
    }

    /// The attempts pooled under `owner`, of actions known by no primary
    /// output.
    pub(crate) fn owned(&self, owner: ConfiguredTarget<&str>) -> Option<&[Attempt]> {
        self.by_owner.get(owner).map(Vec::as_slice)
    }

    /// How many actions known by primary output and owners pooling attempts
    /// this holds.
    pub(crate) fn held(&self) -> usize {
        self.actions.len() + self.by_owner.iter().count()
    }

    /// Each action known by its primary output, with that output's exec path,
    /// the action's owner where the profile named it, and its attempts.
    pub fn actions(&self) -> impl ExactSizeIterator<Item = HeldAction<'_>> {
        self.actions
            .iter()
            .map(|(primary_output, attempted)| held_action(primary_output, attempted))
    }

    /// Each owner of actions known by no primary output, with their attempts.
    pub fn by_owner(&self) -> impl Iterator<Item = (ConfiguredTarget<&str>, &[Attempt])> {
        self.by_owner
            .iter()
            .map(|(owner, attempts)| (owner, attempts.as_slice()))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const ORIGIN_MS: i64 = 1_781_439_245_231;

    fn at(micros: i64) -> EpochMicros {
        EpochMicros::from_millis(ORIGIN_MS).plus_micros(micros)
    }

    pub(crate) fn attempt(start: i64, end: i64, seen: &[(Phase, i64)]) -> Attempt {
        Attempt::new(
            at(start),
            at(end),
            seen.iter().map(|&(phase, micros)| (phase, at(micros))),
        )
    }

    const LOCAL: Phase = Phase::Running(Venue::Local);
    const REMOTE: Phase = Phase::Running(Venue::Remote);

    #[test]
    fn an_attempt_is_pending_then_in_each_phase_entered_then_done() {
        let a = attempt(
            100,
            200,
            &[
                (Phase::CheckingCache, 110),
                (Phase::Scheduling, 120),
                (LOCAL, 130),
            ],
        );
        let states: Vec<State> = [99, 100, 110, 125, 130, 199, 200, 300]
            .into_iter()
            .map(|t| a.state_at(at(t)))
            .collect();
        assert_eq!(
            states,
            [
                State::Pending,
                State::In(Phase::Preparing),
                State::In(Phase::CheckingCache),
                State::In(Phase::Scheduling),
                State::In(LOCAL),
                State::In(LOCAL),
                State::Done(Done::Executed),
                State::Done(Done::Executed),
            ]
        );
        assert_eq!(a.entered(Phase::Preparing), Some(at(100)));
    }

    #[test]
    fn a_phase_seen_after_a_further_one_is_not_entered() {
        let a = attempt(
            0,
            100,
            &[
                (Phase::Scheduling, 10),
                (Phase::CheckingCache, 20),
                (LOCAL, 30),
            ],
        );
        assert_eq!(a.entered(Phase::CheckingCache), None);
        assert_eq!(a.state_at(at(25)), State::In(Phase::Scheduling));
    }

    #[test]
    fn a_phase_is_entered_when_first_seen_within_the_attempt() {
        let a = attempt(
            10,
            100,
            &[
                (LOCAL, 50),
                (LOCAL, 40),
                (Phase::Scheduling, 5),
                (REMOTE, 150),
            ],
        );
        assert_eq!(a.entered(LOCAL), Some(at(40)));
        assert_eq!(a.entered(Phase::Scheduling), None);
        assert_eq!(a.entered(REMOTE), None);
    }

    #[test]
    fn running_moves_between_venues() {
        let a = attempt(0, 100, &[(REMOTE, 10), (LOCAL, 20)]);
        assert_eq!(a.state_at(at(15)), State::In(REMOTE));
        assert_eq!(a.state_at(at(20)), State::In(LOCAL));
        assert_eq!(a.done(), Done::Executed);
    }

    #[test]
    fn an_attempt_that_checked_a_cache_and_never_ran_was_cached() {
        let hit = attempt(0, 100, &[(Phase::CheckingCache, 10)]);
        assert_eq!(
            hit.state_at(at(100)),
            State::Done(Done::Cached(Cache::Spawn(SpawnCache::RemoteOrDisk)))
        );
        let missed = attempt(0, 100, &[(Phase::CheckingCache, 10), (REMOTE, 20)]);
        assert_eq!(missed.done(), Done::Executed);
    }

    #[test]
    fn an_attempt_told_how_it_ended_ends_so_whatever_phases_it_entered() {
        let seen = [(Phase::CheckingCache, at(10)), (LOCAL, at(20))];
        let failed = Attempt::ended(at(0), at(100), seen, Done::Failed);
        assert_eq!(failed.entered(LOCAL), Some(at(20)));
        assert_eq!(failed.state_at(at(100)), State::Done(Done::Failed));
        let up_to_date = Done::Cached(Cache::Action);
        let checked = Attempt::ended(at(0), at(1), [], up_to_date);
        assert_eq!(checked.state_at(at(1)), State::Done(up_to_date));
    }

    #[test]
    fn a_failure_outweighs_every_ending_and_up_to_date_none() {
        let spawn = |cache| Done::Cached(Cache::Spawn(cache));
        let endings = [
            Done::Cached(Cache::Action),
            Done::Executed,
            spawn(SpawnCache::RemoteOrDisk),
            spawn(SpawnCache::Disk),
            Done::Failed,
        ];
        for (i, &less) in endings.iter().enumerate() {
            for &more in &endings[i + 1..] {
                assert_eq!(less.merge(more), more, "{less:?} with {more:?}");
                assert_eq!(more.merge(less), more, "{more:?} with {less:?}");
            }
        }
        let remote = spawn(SpawnCache::Remote);
        assert_eq!(remote.merge(spawn(SpawnCache::Disk)), remote);
    }

    #[test]
    fn an_attempt_ending_before_it_starts_ends_at_its_start() {
        let a = attempt(100, 50, &[]);
        assert_eq!(a.end(), at(100));
        assert_eq!(a.state_at(at(100)), State::Done(Done::Executed));
    }

    #[test]
    fn attempts_are_kept_by_primary_output_and_otherwise_by_owner() {
        let owner = ConfiguredTarget::new("//a", Some("55817e2200e2"));
        let first = attempt(0, 10, &[(LOCAL, 1)]);
        let again = attempt(20, 30, &[(REMOTE, 21)]);
        let mut attempts = Attempts::with_capacity(1);
        assert!(attempts.is_empty());
        attempts.record(Some("a.o"), Some(owner), first);
        attempts.record(Some("a.o"), None, again);
        attempts.record(None, Some(owner), first);
        attempts.record(None, Some(owner), again);
        attempts.record(None, None, first);

        assert_eq!(attempts.action("a.o"), Some(&[first, again][..]));
        assert_eq!(
            attempts.actions().collect::<Vec<_>>(),
            [("a.o", Some(owner), &[first, again][..])]
        );
        assert_eq!(
            attempts.by_owner().collect::<Vec<_>>(),
            [(owner, &[first, again][..])]
        );
    }
}
