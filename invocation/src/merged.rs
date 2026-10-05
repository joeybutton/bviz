//! What the outputs of one invocation tell together of when its actions were
//! executed and how each ended.

use crate::{Attempt, Attempts, ConfiguredTarget, Done};

/// An output of an invocation that reports execution attempts, in the order
/// their timing is trusted, most first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AttemptSource {
    /// Microseconds on the local clock, every phase seen.
    Profile,
    /// Milliseconds on the local clock, phases from their durations.
    ExecutionGraphLog,
    /// Spawns placed by the order they completed in.
    ExecutionLog,
    /// Milliseconds, for the actions a build reports executed.
    BuildEvents,
}

impl AttemptSource {
    /// Every source, most trusted first, at its [`index`](Self::index).
    pub const ALL: [AttemptSource; 4] = [
        AttemptSource::Profile,
        AttemptSource::ExecutionGraphLog,
        AttemptSource::ExecutionLog,
        AttemptSource::BuildEvents,
    ];

    pub const fn index(self) -> usize {
        self as usize
    }
}

/// The attempts each source reports, read together: an action's attempts are
/// timed by the most trusted source holding it, and its last attempt ends as
/// every source holding it tells together, by [`Done::merge`] in the order
/// of [`AttemptSource::ALL`].
#[derive(Clone, Copy, Default)]
pub struct MergedAttempts<'a> {
    /// By [`AttemptSource::index`].
    by_source: [Option<&'a Attempts>; AttemptSource::ALL.len()],
}

impl<'a> MergedAttempts<'a> {
    /// The attempts each source of `reported` reports; a source reported
    /// twice reports what it is paired with last.
    pub fn new(reported: impl IntoIterator<Item = (AttemptSource, &'a Attempts)>) -> Self {
        let mut by_source = [None; AttemptSource::ALL.len()];
        for (source, attempts) in reported {
            by_source[source.index()] = Some(attempts);
        }
        MergedAttempts { by_source }
    }

    /// Each source with what it reports, most trusted first.
    fn reports(&self) -> impl Iterator<Item = (AttemptSource, &'a Attempts)> + use<'a> {
        let by_source = self.by_source;
        AttemptSource::ALL
            .into_iter()
            .filter_map(move |source| Some((source, by_source[source.index()]?)))
    }

    /// Whether no source reports an attempt.
    pub fn is_empty(&self) -> bool {
        self.reports().all(|(_, attempts)| attempts.is_empty())
    }

    /// How many actions and owners the sources hold between them, each
    /// counted once per source holding it: at least as many as
    /// [`actions`](Self::actions) and [`by_owner`](Self::by_owner) yield.
    pub fn held(&self) -> usize {
        self.reports().map(|(_, attempts)| attempts.held()).sum()
    }

    /// The attempts of the action whose primary output has the exec path
    /// `primary_output`.
    pub fn action(&self, primary_output: &str) -> Option<ActionAttempts<'a>> {
        let (source, primary_output, attempts) =
            self.reports().find_map(|(source, attempts)| {
                let (primary_output, _, tried) = attempts.attempted(primary_output)?;
                Some((source, primary_output, tried))
            })?;
        Some(self.told(primary_output, source, attempts))
    }

    /// Each action known by its primary output, with that output's exec path,
    /// its owner as the most trusted source naming one names it, and its
    /// attempts.
    pub fn actions(
        &self,
    ) -> impl Iterator<
        Item = (
            &'a str,
            Option<ConfiguredTarget<&'a str>>,
            ActionAttempts<'a>,
        ),
    > + use<'a> {
        let merged = *self;
        self.reports().flat_map(move |(source, attempts)| {
            attempts
                .actions()
                .filter(move |&(primary_output, _, _)| {
                    !merged
                        .before(source)
                        .any(|(_, more)| more.attempted(primary_output).is_some())
                })
                .map(move |(primary_output, owner, tried)| {
                    let owner = owner.or_else(|| {
                        merged
                            .after(source)
                            .find_map(|(_, less)| less.attempted(primary_output)?.1)
                    });
                    (
                        primary_output,
                        owner,
                        merged.told(primary_output, source, tried),
                    )
                })
        })
    }

    /// Each owner of actions known by no primary output, with their attempts
    /// as the most trusted source pooling attempts under it reports them.
    pub fn by_owner(
        &self,
    ) -> impl Iterator<Item = (ConfiguredTarget<&'a str>, &'a [Attempt])> + use<'a> {
        let merged = *self;
        self.reports().flat_map(move |(source, attempts)| {
            attempts.by_owner().filter(move |&(owner, _)| {
                !merged
                    .before(source)
                    .any(|(_, more)| more.owned(owner).is_some())
            })
        })
    }

    /// The sources trusted more than `source`, with what they report.
    fn before(
        &self,
        source: AttemptSource,
    ) -> impl Iterator<Item = (AttemptSource, &'a Attempts)> + use<'a> {
        self.reports().take_while(move |&(other, _)| other < source)
    }

    /// The sources trusted less than `source`, with what they report.
    fn after(
        &self,
        source: AttemptSource,
    ) -> impl Iterator<Item = (AttemptSource, &'a Attempts)> + use<'a> {
        self.reports()
            .skip_while(move |&(other, _)| other <= source)
    }

    /// The attempts `timed`, which `source` reports of the action writing
    /// `primary_output`.
    fn told(
        &self,
        primary_output: &'a str,
        source: AttemptSource,
        timed: &'a [Attempt],
    ) -> ActionAttempts<'a> {
        ActionAttempts {
            source,
            timed,
            primary_output,
            merged: *self,
        }
    }
}

/// The attempts of one action, as the sources holding it tell them together.
#[derive(Clone, Copy)]
pub struct ActionAttempts<'a> {
    /// The most trusted source holding the action, which times its attempts.
    source: AttemptSource,
    timed: &'a [Attempt],
    primary_output: &'a str,
    /// The sources that tell how the last attempt ended.
    merged: MergedAttempts<'a>,
}

impl<'a> ActionAttempts<'a> {
    /// The source the attempts are timed by.
    pub fn source(&self) -> AttemptSource {
        self.source
    }

    pub fn len(&self) -> usize {
        self.timed.len()
    }

    pub fn is_empty(&self) -> bool {
        self.timed.is_empty()
    }

    /// The attempts in the order recorded, the last ending as every source
    /// holding the action tells.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = Attempt> + use<'a> {
        let ActionAttempts {
            timed,
            primary_output,
            merged,
            ..
        } = *self;
        let final_index = timed.len().saturating_sub(1);
        timed
            .iter()
            .enumerate()
            .map(move |(index, &attempt)| match index == final_index {
                true => attempt.ending(
                    merged
                        .reports()
                        .filter_map(|(_, attempts)| attempts.attempted(primary_output)?.2.last())
                        .map(Attempt::done)
                        .fold(attempt.done(), Done::merge),
                ),
                false => attempt,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attempts::tests::attempt;
    use crate::{Cache, Phase, SpawnCache, Venue};

    const LOCAL: Phase = Phase::Running(Venue::Local);

    fn ended(start: i64, end: i64, done: Done) -> Attempt {
        attempt(start, end, &[(LOCAL, start)]).ending(done)
    }

    fn reported(actions: &[(&str, Option<&str>, &[Attempt])]) -> Attempts {
        let mut attempts = Attempts::with_capacity(actions.len());
        for &(primary_output, owner, tried) in actions {
            let owner = owner.map(|label| ConfiguredTarget::new(label, None));
            for &attempt in tried {
                attempts.record(Some(primary_output), owner, attempt);
            }
        }
        attempts
    }

    #[test]
    fn an_action_is_timed_by_the_most_trusted_source_holding_it() {
        let profiled = ended(0, 10, Done::Executed);
        let logged = ended(5, 20, Done::Executed);
        let profile = reported(&[("a.o", None, &[profiled])]);
        let log = reported(&[("a.o", None, &[logged]), ("b.o", None, &[logged])]);
        let merged = MergedAttempts::new([
            (AttemptSource::ExecutionLog, &log),
            (AttemptSource::Profile, &profile),
        ]);
        let a = merged.action("a.o").expect("held");
        assert_eq!(a.source(), AttemptSource::Profile);
        assert_eq!(a.iter().collect::<Vec<_>>(), [profiled]);
        let b = merged.action("b.o").expect("held");
        assert_eq!(b.source(), AttemptSource::ExecutionLog);
        assert_eq!(b.iter().collect::<Vec<_>>(), [logged]);

        let mut actions: Vec<_> = merged
            .actions()
            .map(|(output, _, attempts)| (output, attempts.source()))
            .collect();
        actions.sort_unstable();
        assert_eq!(
            actions,
            [
                ("a.o", AttemptSource::Profile),
                ("b.o", AttemptSource::ExecutionLog)
            ]
        );
        assert!(merged.held() >= actions.len());
    }

    #[test]
    fn the_last_attempt_ends_as_every_source_tells_together() {
        let hit = Done::Cached(Cache::Spawn(SpawnCache::RemoteOrDisk));
        let disk = Done::Cached(Cache::Spawn(SpawnCache::Disk));
        let rewound = [ended(0, 10, Done::Executed), ended(20, 30, hit)];
        let profile = reported(&[("a.o", None, &rewound), ("b.o", None, &rewound)]);
        let log = reported(&[
            ("a.o", None, &[ended(25, 30, disk)]),
            ("b.o", None, &[ended(25, 30, Done::Failed)]),
        ]);
        let merged = MergedAttempts::new([
            (AttemptSource::Profile, &profile),
            (AttemptSource::ExecutionLog, &log),
        ]);
        let endings = |output| {
            merged
                .action(output)
                .expect("held")
                .iter()
                .map(|attempt| (attempt.start(), attempt.done()))
                .collect::<Vec<_>>()
        };
        let starts = rewound.map(|attempt| attempt.start());
        assert_eq!(
            endings("a.o"),
            [(starts[0], Done::Executed), (starts[1], disk)]
        );
        assert_eq!(
            endings("b.o"),
            [(starts[0], Done::Executed), (starts[1], Done::Failed)]
        );
    }

    #[test]
    fn an_action_is_owned_as_the_most_trusted_source_naming_an_owner_names_it() {
        let tried = [ended(0, 10, Done::Executed)];
        let profile = reported(&[("a.o", None, &tried)]);
        let log = reported(&[("a.o", Some("//a"), &tried)]);
        let merged = MergedAttempts::new([
            (AttemptSource::Profile, &profile),
            (AttemptSource::ExecutionLog, &log),
        ]);
        let owners: Vec<_> = merged.actions().map(|(_, owner, _)| owner).collect();
        assert_eq!(owners, [Some(ConfiguredTarget::new("//a", None))]);
    }

    #[test]
    fn an_owner_pools_the_attempts_of_the_most_trusted_source_pooling_any() {
        let owner = ConfiguredTarget::new("//a", None);
        let profiled = ended(0, 10, Done::Executed);
        let logged = ended(5, 20, Done::Failed);
        let mut profile = Attempts::default();
        profile.record(None, Some(owner), profiled);
        let mut log = Attempts::default();
        log.record(None, Some(owner), logged);
        let other = ConfiguredTarget::new("//b", None);
        log.record(None, Some(other), logged);
        let merged = MergedAttempts::new([
            (AttemptSource::Profile, &profile),
            (AttemptSource::ExecutionLog, &log),
        ]);
        let mut pooled: Vec<_> = merged.by_owner().collect();
        pooled.sort_unstable_by_key(|(owner, _)| owner.label());
        assert_eq!(pooled, [(owner, &[profiled][..]), (other, &[logged][..])]);
    }

    #[test]
    fn no_source_reporting_an_attempt_merges_to_none() {
        let empty = Attempts::default();
        assert!(MergedAttempts::default().is_empty());
        assert!(MergedAttempts::new([(AttemptSource::Profile, &empty)]).is_empty());
        assert_eq!(MergedAttempts::default().actions().count(), 0);
    }
}
