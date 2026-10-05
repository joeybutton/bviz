//! What the outputs of one invocation tell together of when its actions were
//! executed and how each ended.

use std::collections::HashMap;

use crate::attempts::DescribedAction;
use crate::{Attempt, Attempts, Cache, ConfiguredTarget, Done, HeldAction};

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
/// of [`AttemptSource::ALL`]. A source holds an action by its primary output,
/// or by a description [resolved](Self::resolved) to that output.
#[derive(Clone, Copy, Default)]
pub struct MergedAttempts<'a> {
    /// By [`AttemptSource::index`].
    by_source: [Option<&'a Attempts>; AttemptSource::ALL.len()],
    resolved: Option<&'a Resolved<'a>>,
}

/// The primary output of each action that the sources of a
/// [`MergedAttempts`] know by its description and that a reader knowing the
/// build's actions found.
pub struct Resolved<'a> {
    outputs: HashMap<DescribedAction<&'a str>, &'a str>,
    /// By primary output, what each source, at its [`AttemptSource::index`],
    /// holds by description of the action writing it.
    held: HashMap<&'a str, [Option<Holding<'a>>; AttemptSource::ALL.len()]>,
}

type Described<'a> = (DescribedAction<&'a str>, &'a [Attempt]);

/// What one source holds by description of the action writing one output.
#[derive(Clone, Copy)]
enum Holding<'a> {
    One(Described<'a>),
    /// Several descriptions the source gives were found to write the output,
    /// which cannot tell which of them did, so none is held by it.
    Several,
}

impl<'a> Holding<'a> {
    fn one(self) -> Option<Described<'a>> {
        match self {
            Holding::One(described) => Some(described),
            Holding::Several => None,
        }
    }
}

impl<'a> Resolved<'a> {
    /// The action `primary_output` names as `source` holds it by
    /// description.
    fn held(&self, source: AttemptSource, primary_output: &str) -> Option<HeldAction<'a>> {
        let (&primary_output, by_source) = self.held.get_key_value(primary_output)?;
        let (described, tried) = by_source[source.index()]?.one()?;
        Some((primary_output, Some(described.owner_target()), tried))
    }

    /// Whether `described` stands among the actions `source` holds by
    /// primary output.
    fn resolves(&self, source: AttemptSource, described: DescribedAction<&str>) -> bool {
        self.outputs
            .get(&described)
            .and_then(|primary_output| self.held.get(primary_output)?[source.index()]?.one())
            .is_some_and(|(holding, _)| holding == described)
    }

    /// Each action `source` holds by a description resolved to its primary
    /// output.
    fn actions(&self, source: AttemptSource) -> impl Iterator<Item = HeldAction<'a>> + use<'_, 'a> {
        self.held
            .iter()
            .filter_map(move |(&primary_output, by_source)| {
                let (described, tried) = by_source[source.index()]?.one()?;
                Some((primary_output, Some(described.owner_target()), tried))
            })
    }
}

impl<'a> MergedAttempts<'a> {
    /// The attempts each source of `reported` reports; a source reported
    /// twice reports what it is paired with last.
    pub fn new(reported: impl IntoIterator<Item = (AttemptSource, &'a Attempts)>) -> Self {
        let mut by_source = [None; AttemptSource::ALL.len()];
        for (source, attempts) in reported {
            by_source[source.index()] = Some(attempts);
        }
        MergedAttempts {
            by_source,
            resolved: None,
        }
    }

    /// Finds the primary output of each action a source knows by its
    /// description as `output` finds it, `None` for one it does not. Of the
    /// actions one source describes that `output` finds the same output for,
    /// none is held by that output.
    pub fn resolve(
        &self,
        mut output: impl FnMut(DescribedAction<&'a str>) -> Option<&'a str>,
    ) -> Resolved<'a> {
        let described: usize = self
            .reports()
            .map(|(_, attempts)| attempts.described().len())
            .sum();
        let mut resolved = Resolved {
            outputs: HashMap::with_capacity(described),
            held: HashMap::with_capacity(described),
        };
        for (source, attempts) in self.reports() {
            for (action, tried) in attempts.described() {
                let found = match resolved.outputs.get(&action) {
                    Some(&primary_output) => Some(primary_output),
                    None => output(action),
                };
                let Some(primary_output) = found else {
                    continue;
                };
                resolved.outputs.insert(action, primary_output);
                let by_source = resolved
                    .held
                    .entry(primary_output)
                    .or_insert([None; AttemptSource::ALL.len()]);
                let holding = &mut by_source[source.index()];
                *holding = Some(match holding {
                    None => Holding::One((action, tried)),
                    Some(_) => Holding::Several,
                });
            }
        }
        resolved
    }

    /// Each action a source knows by its description, once for each source
    /// describing it.
    pub fn described(&self) -> impl Iterator<Item = DescribedAction<&'a str>> + use<'a> {
        self.reports()
            .flat_map(|(_, attempts)| attempts.described().map(|(action, _)| action))
    }

    /// These attempts, with each action a source knows by a description in
    /// `resolved` held by its primary output.
    pub fn resolved(self, resolved: &'a Resolved<'a>) -> Self {
        MergedAttempts {
            resolved: Some(resolved),
            ..self
        }
    }

    /// Each source with what it reports, most trusted first.
    fn reports(&self) -> impl Iterator<Item = (AttemptSource, &'a Attempts)> + use<'a> {
        let by_source = self.by_source;
        AttemptSource::ALL
            .into_iter()
            .filter_map(move |source| Some((source, by_source[source.index()]?)))
    }

    /// How an action whose last attempt, `last`, the sources together tell
    /// ended as `done` ended. Bazel profiles every action it does not find up
    /// to date:
    /// <https://github.com/bazelbuild/bazel/blob/9.1.0/src/main/java/com/google/devtools/build/lib/skyframe/SkyframeActionExecutor.java#L1075-L1082>
    /// so an action seen to end [without a spawn](Done::Spawnless) where a
    /// source holding [every such action](Attempts::is_exhaustive) reports,
    /// and so holds no attempt of it, was found up to date. An execution graph
    /// log records an action Skyframe pruned as ending at the instant it
    /// starts, so one that took any time the action cache found up to date:
    /// <https://github.com/bazelbuild/bazel/blob/9.1.0/src/main/java/com/google/devtools/build/lib/runtime/ExecutionGraphModule.java#L325-L342>
    fn corroborated(&self, last: Attempt, done: Done) -> Done {
        let exhaustive = self.reports().any(|(_, attempts)| attempts.is_exhaustive());
        match done {
            Done::Spawnless if exhaustive && last.start() < last.end() => {
                Done::Cached(Cache::Action)
            }
            Done::Spawnless if exhaustive => Done::UpToDate,
            done => done,
        }
    }

    /// The action whose primary output has the exec path `primary_output`, as
    /// `attempts`, which `source` reports, holds it.
    fn holding(
        &self,
        source: AttemptSource,
        attempts: &'a Attempts,
        primary_output: &str,
    ) -> Option<HeldAction<'a>> {
        attempts
            .attempted(primary_output)
            .or_else(|| self.resolved?.held(source, primary_output))
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
                let (primary_output, _, tried) = self.holding(source, attempts, primary_output)?;
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
            let described = merged
                .resolved
                .into_iter()
                .flat_map(move |resolved| resolved.actions(source))
                .filter(move |&(primary_output, _, _)| {
                    attempts.attempted(primary_output).is_none()
                });
            attempts
                .actions()
                .chain(described)
                .filter(move |&(primary_output, _, _)| {
                    !merged.before(source).any(|(more_source, more)| {
                        merged.holding(more_source, more, primary_output).is_some()
                    })
                })
                .map(move |(primary_output, owner, tried)| {
                    let owner = owner.or_else(|| {
                        merged.after(source).find_map(|(less_source, less)| {
                            merged.holding(less_source, less, primary_output)?.1
                        })
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
    /// as the most trusted source pooling attempts under it reports them. An
    /// action a source knows by a description not resolved to a primary
    /// output is pooled under its owner apart from the rest, so an owner may
    /// come more than once.
    pub fn by_owner(
        &self,
    ) -> impl Iterator<Item = (ConfiguredTarget<&'a str>, &'a [Attempt])> + use<'a> {
        let merged = *self;
        self.reports().flat_map(move |(source, attempts)| {
            let described = attempts
                .described()
                .filter(move |&(described, _)| {
                    !merged
                        .resolved
                        .is_some_and(|resolved| resolved.resolves(source, described))
                })
                .map(|(described, tried)| (described.owner_target(), tried));
            attempts
                .by_owner()
                .chain(described)
                .filter(move |&(owner, _)| {
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
    /// holding the action tells, [corroborated](MergedAttempts::corroborated)
    /// by a source holding every action Bazel did not find up to date.
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
                    merged.corroborated(
                        attempt,
                        merged
                            .reports()
                            .filter_map(|(source, attempts)| {
                                merged.holding(source, attempts, primary_output)?.2.last()
                            })
                            .map(Attempt::done)
                            .fold(attempt.done(), Done::merge),
                    ),
                ),
                false => attempt,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attempts::tests::attempt;
    use crate::{Phase, SpawnCache, Venue};

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
    fn an_action_ending_without_a_spawn_is_up_to_date_where_an_exhaustive_source_holds_none() {
        let took = ended(0, 10, Done::Spawnless);
        let pruned = ended(20, 20, Done::Spawnless);
        let log = reported(&[("a.o", None, &[took]), ("p.o", None, &[pruned])]);
        let ending = |merged: MergedAttempts<'_>, output| {
            let held = merged.action(output).expect("held");
            held.iter()
                .map(|attempt| attempt.done())
                .collect::<Vec<_>>()
        };
        let alone = MergedAttempts::new([(AttemptSource::ExecutionGraphLog, &log)]);
        assert_eq!(ending(alone, "a.o"), [Done::Spawnless]);
        let mut profile = reported(&[("b.o", None, &[ended(0, 10, Done::Executed)])]);
        let partial = MergedAttempts::new([
            (AttemptSource::Profile, &profile),
            (AttemptSource::ExecutionGraphLog, &log),
        ]);
        assert_eq!(ending(partial, "a.o"), [Done::Spawnless]);
        profile.mark_exhaustive();
        let exhaustive = MergedAttempts::new([
            (AttemptSource::Profile, &profile),
            (AttemptSource::ExecutionGraphLog, &log),
        ]);
        assert_eq!(ending(exhaustive, "a.o"), [Done::Cached(Cache::Action)]);
        assert_eq!(ending(exhaustive, "p.o"), [Done::UpToDate]);
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

    const DESCRIBED: DescribedAction<&str> = DescribedAction {
        owner: "//a",
        mnemonic: "Genrule",
        description: "Executing genrule //a",
    };

    fn described(tried: &[Attempt]) -> Attempts {
        let mut attempts = Attempts::default();
        for &attempt in tried {
            attempts.record_described(DESCRIBED, attempt);
        }
        attempts
    }

    #[test]
    fn a_described_action_resolved_to_an_output_is_held_by_it() {
        let up_to_date = ended(0, 10, Done::Cached(Cache::Action));
        let log = described(&[up_to_date]);
        let merged = MergedAttempts::new([(AttemptSource::ExecutionGraphLog, &log)]);
        let resolved = merged.resolve(|action| (action == DESCRIBED).then_some("a.o"));
        let merged = merged.resolved(&resolved);
        let actions: Vec<_> = merged
            .actions()
            .map(|(output, owner, attempts)| (output, owner, attempts.iter().collect::<Vec<_>>()))
            .collect();
        let owner = ConfiguredTarget::new("//a", None);
        assert_eq!(actions, [("a.o", Some(owner), vec![up_to_date])]);
        assert_eq!(merged.by_owner().count(), 0);
    }

    #[test]
    fn a_described_action_is_timed_as_any_action_its_output_names() {
        let profiled = ended(0, 10, Done::Executed);
        let profile = reported(&[("a.o", None, &[profiled])]);
        let log = described(&[ended(1, 9, Done::Cached(Cache::Action))]);
        let merged = MergedAttempts::new([
            (AttemptSource::Profile, &profile),
            (AttemptSource::ExecutionGraphLog, &log),
        ]);
        let resolved = merged.resolve(|_| Some("a.o"));
        let merged = merged.resolved(&resolved);
        let actions: Vec<_> = merged
            .actions()
            .map(|(output, owner, attempts)| (output, owner, attempts.source()))
            .collect();
        let owner = Some(ConfiguredTarget::new("//a", None));
        assert_eq!(actions, [("a.o", owner, AttemptSource::Profile)]);
        let a = merged.action("a.o").expect("held");
        assert_eq!(a.iter().collect::<Vec<_>>(), [profiled]);
    }

    #[test]
    fn descriptions_found_to_write_one_output_are_pooled_under_their_owner() {
        let other = DescribedAction {
            description: "Executing genrule //a again",
            ..DESCRIBED
        };
        let tried = [ended(0, 10, Done::Spawnless)];
        let mut log = described(&tried);
        log.record_described(other, tried[0]);
        let merged = MergedAttempts::new([(AttemptSource::ExecutionGraphLog, &log)]);
        let resolved = merged.resolve(|_| Some("a.o"));
        let merged = merged.resolved(&resolved);
        assert_eq!(merged.actions().count(), 0);
        assert!(merged.action("a.o").is_none());
        assert_eq!(merged.by_owner().count(), 2);
    }

    #[test]
    fn a_described_action_not_resolved_is_pooled_under_its_owner() {
        let tried = [ended(0, 10, Done::Cached(Cache::Action))];
        let log = described(&tried);
        let merged = MergedAttempts::new([(AttemptSource::ExecutionGraphLog, &log)]);
        let owner = ConfiguredTarget::new("//a", None);
        assert_eq!(merged.by_owner().collect::<Vec<_>>(), [(owner, &tried[..])]);
        let resolved = merged.resolve(|_| None);
        let merged = merged.resolved(&resolved);
        assert_eq!(merged.by_owner().collect::<Vec<_>>(), [(owner, &tried[..])]);
        assert_eq!(merged.actions().count(), 0);
    }

    #[test]
    fn no_source_reporting_an_attempt_merges_to_none() {
        let empty = Attempts::default();
        assert!(MergedAttempts::default().is_empty());
        assert!(MergedAttempts::new([(AttemptSource::Profile, &empty)]).is_empty());
        assert_eq!(MergedAttempts::default().actions().count(), 0);
    }
}
