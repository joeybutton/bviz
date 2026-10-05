//! An execution log's spawns as execution attempts on the local wall clock.
//!
//! Bazel writes a spawn to the log when it completes, so the log lists spawns
//! in the order they ended. Only a spawn run on this machine has a start on
//! its clock; the others are placed between those by that order.

use std::borrow::Cow;
use std::time::Duration;

use super::{LoggedSpawn, LoggedSpawns, Phase as Timed};
use crate::{
    Attempt, Attempts, Cache, ConfiguredTarget, Done, EpochMicros, Interval, Phase, SpawnCache,
    TEST_LOG, Venue,
};

/// The mnemonic of a test's action, whose primary output, the test's log, is
/// no output of its spawns: they write beside it.
const TEST_MNEMONIC: &str = "TestRunner";

/// The file a test's own run declares beside its log, which the spawns
/// processing that run's results after it, generating its XML report or
/// post-processing its coverage, do not:
/// <https://github.com/bazelbuild/bazel/blob/9.1.0/src/main/java/com/google/devtools/build/lib/analysis/test/TestRunnerAction.java#L377-L378>
/// <https://github.com/bazelbuild/bazel/blob/9.1.0/src/main/java/com/google/devtools/build/lib/exec/StandaloneTestStrategy.java#L446-L517>
const TEST_RUN_OUTPUT: &str = "test.exited_prematurely";

/// What answered a spawn, as its runner name and cache-hit flag tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answered {
    /// Run on this machine, which the log gives a start of on the local
    /// clock.
    Here(Ran),
    /// Sent to a remote executor, which reports how long the spawn queued and
    /// how long its worker set it up, ran it and uploaded its outputs, all
    /// before Bazel fetched them:
    /// <https://github.com/bazelbuild/bazel/blob/9.1.0/src/main/java/com/google/devtools/build/lib/remote/RemoteSpawnRunner.java#L433-L465>
    RemoteExecution,
    Cache(SpawnCache),
    /// A runner whose phases the log does not tell, such as a spawn reusing
    /// the outputs of an identical one running in the same build.
    Unknown,
}

/// A runner of spawns on this machine. Each reports scheduling, waits for
/// local resources, and reports running, and its setup falls around those
/// in a way of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ran {
    /// A subprocess run unsandboxed, whose setup is mostly the inputs it
    /// fetches before it reports scheduling, and whose logged start is its
    /// subprocess's, which ends with execution and processing the outputs:
    /// <https://github.com/bazelbuild/bazel/blob/9.1.0/src/main/java/com/google/devtools/build/lib/exec/local/LocalSpawnRunner.java#L131-L150>
    Unsandboxed,
    /// A subprocess run in a sandbox, whose setup is mostly building the
    /// sandbox once it reports running, and whose logged start is its
    /// subprocess's:
    /// <https://github.com/bazelbuild/bazel/blob/9.1.0/src/main/java/com/google/devtools/build/lib/sandbox/AbstractSandboxSpawnRunner.java#L99-L143>
    Sandboxed,
    /// A persistent worker, whose setup is mostly staging the request's
    /// inputs before it waits, and whose logged start is the spawn's own,
    /// which its total time runs from:
    /// <https://github.com/bazelbuild/bazel/blob/9.1.0/src/main/java/com/google/devtools/build/lib/worker/WorkerSpawnRunner.java#L160-L225>
    Worker,
}

impl Answered {
    fn of(spawn: &LoggedSpawn<'_>) -> Answered {
        if spawn.cache_hit() {
            return Answered::Cache(SpawnCache::answering(spawn.runner()));
        }
        match spawn.runner() {
            "local" => Answered::Here(Ran::Unsandboxed),
            "linux-sandbox"
            | "darwin-sandbox"
            | "processwrapper-sandbox"
            | "windows-sandbox"
            | "docker" => Answered::Here(Ran::Sandboxed),
            "worker" => Answered::Here(Ran::Worker),
            "remote" => Answered::RemoteExecution,
            _ => Answered::Unknown,
        }
    }
}

/// When `spawn` ended on the local clock, where its log says: a spawn run
/// here whose start is set and after the epoch.
fn anchored_end(spawn: &LoggedSpawn<'_>) -> Option<EpochMicros> {
    let Answered::Here(ran) = Answered::of(spawn) else {
        return None;
    };
    let start = spawn.start().filter(|start| start.micros() > 0)?;
    let timing = spawn.timing();
    let lasted = match ran {
        Ran::Unsandboxed | Ran::Sandboxed => {
            timing.phase(Timed::Execute) + timing.phase(Timed::ProcessOutputs)
        }
        Ran::Worker => timing.total(),
    };
    Some(start.after(lasted))
}

/// Where the spawns of a log end on the local clock.
struct Ends {
    /// In log order.
    ends: Vec<EpochMicros>,
    /// The end of the first spawn the log gives a start on the local clock.
    anchor: EpochMicros,
    /// Whether any end is interpolated.
    interpolated: bool,
}

/// The ends of the spawns of a log in log order, `anchored` giving where the
/// log places each it places on the local clock. Each other is interpolated
/// by its place in the log between the nearest placed before and after it,
/// these taken as the latest end placed so far so that interpolated ends
/// never decrease along the log. Before the first and after the last placed
/// spawn, ends move on at the mean pace the placed spawns ended at along the
/// log, or not at all where only one is placed. `None` where no spawn is
/// placed.
fn ends(anchored: &[Option<EpochMicros>]) -> Option<Ends> {
    let mut anchors: Vec<(usize, i64)> = Vec::with_capacity(anchored.len());
    let mut latest = i64::MIN;
    for (index, end) in anchored.iter().enumerate() {
        if let Some(end) = end {
            latest = latest.max(end.micros());
            anchors.push((index, latest));
        }
    }
    let (&first, &last) = (anchors.first()?, anchors.last()?);
    let pace_between = |from: (usize, i64), to: (usize, i64)| match to.0 - from.0 {
        0 => 0.0,
        steps => (to.1 - from.1) as f64 / steps as f64,
    };
    let mean_pace = pace_between(first, last);
    let along = |from: (usize, i64), index: usize, pace: f64| {
        let steps = index as f64 - from.0 as f64;
        EpochMicros::from_micros(from.1).plus_micros((steps * pace).round() as i64)
    };
    let mut ends = Vec::with_capacity(anchored.len());
    // How many anchors lie before the spawn at hand.
    let mut passed: usize = 0;
    for (index, end) in anchored.iter().enumerate() {
        if let Some(end) = end {
            ends.push(*end);
            passed += 1;
            continue;
        }
        ends.push(match passed.checked_sub(1).map(|before| anchors[before]) {
            None => along(first, index, mean_pace),
            Some(before) => match anchors.get(passed) {
                Some(&after) => along(before, index, pace_between(before, after)),
                None => along(before, index, mean_pace),
            },
        });
    }
    Some(Ends {
        ends,
        anchor: EpochMicros::from_micros(first.1),
        interpolated: anchors.len() < anchored.len(),
    })
}

impl<'a> LoggedSpawn<'a> {
    /// The attempt the spawn makes, lasting its total time up to `end` and
    /// entering the phases its timing tells, counted back from its end so
    /// that time its timing does not place falls at its start. One Bazel ran
    /// here is scheduled while it waits for local resources and running from
    /// then on, its setup falling as its [runner](Ran) has it; a remote
    /// execution checks the cache once parsed, is scheduled while queued, and
    /// is running from its executor's setup until its outputs are fetched.
    pub fn attempt(&self, end: EpochMicros) -> Attempt {
        let timing = self.timing();
        let took = |phases: &[Timed]| {
            phases.iter().fold(Duration::ZERO, |sum, &phase| {
                sum.saturating_add(timing.phase(phase))
            })
        };
        let start = end.before(timing.total());
        let within = |at: EpochMicros| at.clamp(start, end.max(start));
        let answered = Answered::of(self);
        let seen = match answered {
            Answered::Here(ran) => {
                let (running, waiting) = match ran {
                    Ran::Unsandboxed => (
                        took(&[Timed::Execute, Timed::ProcessOutputs]),
                        took(&[Timed::Queue]),
                    ),
                    Ran::Sandboxed => (
                        took(&[Timed::Setup, Timed::Execute, Timed::ProcessOutputs]),
                        took(&[Timed::Queue]),
                    ),
                    Ran::Worker => (
                        took(&[Timed::Execute, Timed::ProcessOutputs]),
                        took(&[Timed::Setup, Timed::Queue]),
                    ),
                };
                let running = end.before(running);
                [
                    Some((Phase::Scheduling, within(running.before(waiting)))),
                    Some((Phase::Running(Venue::Local), within(running))),
                    None,
                ]
            }
            Answered::RemoteExecution => {
                let running = end.before(took(&[
                    Timed::Fetch,
                    Timed::ProcessOutputs,
                    Timed::Execute,
                    Timed::Setup,
                ]));
                [
                    Some((
                        Phase::CheckingCache,
                        within(start.after(took(&[Timed::Parse]))),
                    )),
                    Some((
                        Phase::Scheduling,
                        within(running.before(took(&[Timed::Queue]))),
                    )),
                    Some((Phase::Running(Venue::Remote), within(running))),
                ]
            }
            Answered::Cache(_) => [Some((Phase::CheckingCache, start)), None, None],
            Answered::Unknown => [None; 3],
        };
        let done = match answered {
            _ if self.failed() => Done::Failed,
            Answered::Cache(cache) => Done::Cached(Cache::Spawn(cache)),
            _ => Done::Executed,
        };
        Attempt::ended(start, end, seen.into_iter().flatten(), done)
    }

    /// The exec path of the primary output of the spawn's action: the spawn's
    /// own, but for a test's spawn the test's log, beside which its spawns
    /// write.
    pub fn action_output(&self) -> Option<Cow<'a, str>> {
        let output = self.primary_output()?;
        if self.mnemonic() != TEST_MNEMONIC {
            return Some(Cow::Borrowed(output));
        }
        Some(Cow::Owned(match output.rsplit_once('/') {
            Some((directory, _)) => format!("{directory}/{TEST_LOG}"),
            None => TEST_LOG.to_owned(),
        }))
    }

    /// Whether the spawn processes the results of a test's run, after it and
    /// as part of it, rather than running the test.
    pub fn follows_a_test_run(&self) -> bool {
        self.mnemonic() == TEST_MNEMONIC
            && !self
                .outputs()
                .any(|output| output.rsplit('/').next() == Some(TEST_RUN_OUTPUT))
    }
}

/// The execution attempts an execution log's spawns make, on the local wall
/// clock.
pub struct DatedSpawns {
    pub attempts: Attempts,
    pub placement: Placement,
}

/// Where the attempts of [`DatedSpawns`] stand on the wall clock together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    span: Interval,
    interpolated: bool,
}

impl Placement {
    /// From the start of the first attempt to the end of the last.
    pub fn span(&self) -> Interval {
        self.span
    }

    /// Whether any attempt is placed by its place in the log rather than by
    /// a start on the local clock.
    pub fn interpolated(&self) -> bool {
        self.interpolated
    }
}

impl LoggedSpawns {
    /// Each spawn as an [attempt](LoggedSpawn::attempt) of the action writing
    /// its [action output](LoggedSpawn::action_output), owned by the spawn's
    /// target in no particular configuration, ending where the log places
    /// it; one that [follows a test's run](LoggedSpawn::follows_a_test_run)
    /// as part of the attempt of that run. `None` where no spawn ran here with
    /// a start on the local clock to place the log by.
    pub fn dated(&self) -> Option<DatedSpawns> {
        let anchored: Vec<Option<EpochMicros>> =
            self.iter().map(|spawn| anchored_end(&spawn)).collect();
        let Ends {
            ends,
            anchor,
            interpolated,
        } = ends(&anchored)?;
        let mut attempts = Attempts::with_capacity(self.len());
        let mut span = Interval::at(anchor);
        for (spawn, ended) in self.iter().zip(ends) {
            let attempt = spawn.attempt(ended);
            span = span.hull(attempt.span());
            let owner = spawn
                .target_label()
                .map(|label| ConfiguredTarget::new(label, None));
            let output = spawn.action_output();
            match spawn.follows_a_test_run() {
                true => attempts.record_continued(output.as_deref(), owner, attempt),
                false => attempts.record(output.as_deref(), owner, attempt),
            }
        }
        Some(DatedSpawns {
            attempts,
            placement: Placement { span, interpolated },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::State;
    use crate::spawn::{Builder, Logged, PhaseTiming, REPORTED_PHASES};

    const ORIGIN_MS: i64 = 1_781_439_245_231;
    const LOCAL: Phase = Phase::Running(Venue::Local);
    const REMOTE: Phase = Phase::Running(Venue::Remote);

    /// The instant `millis` milliseconds into the build.
    fn at(millis: i64) -> EpochMicros {
        EpochMicros::from_millis(ORIGIN_MS + millis)
    }

    /// A spawn writing `output`, which `runner` answered after `total`
    /// milliseconds, spending `phases` in each phase.
    struct Spawn<'a> {
        runner: &'a str,
        mnemonic: &'a str,
        target_label: &'a str,
        outputs: &'a [&'a str],
        cache_hit: bool,
        total: u64,
        phases: &'a [(Timed, u64)],
        start: Option<EpochMicros>,
        exit_code: i32,
    }

    fn spawn<'a>(runner: &'a str, output: &'a [&'a str], total: u64) -> Spawn<'a> {
        Spawn {
            runner,
            mnemonic: "Genrule",
            target_label: "//p:t",
            outputs: output,
            cache_hit: false,
            total,
            phases: &[],
            start: None,
            exit_code: 0,
        }
    }

    fn log(spawns: &[Spawn]) -> LoggedSpawns {
        let mut builder = Builder::with_capacity(spawns.len(), spawns.len(), 0);
        for spawn in spawns {
            for output in spawn.outputs {
                builder.output(output);
            }
            let mut reported = [Duration::ZERO; REPORTED_PHASES];
            for &(phase, millis) in spawn.phases {
                reported[phase.index()] = Duration::from_millis(millis);
            }
            if let Some(start) = spawn.start {
                builder.started(start);
            }
            builder.exited(spawn.exit_code, None);
            builder.push(Logged {
                runner: spawn.runner,
                mnemonic: spawn.mnemonic,
                target_label: spawn.target_label,
                cache_hit: spawn.cache_hit,
                timing: PhaseTiming::new(Duration::from_millis(spawn.total), reported),
            });
        }
        builder.finish()
    }

    /// How long [`sandboxed`] spawns run, in milliseconds.
    const SANDBOXED_RUN: u64 = 100;

    /// A sandboxed spawn whose subprocess started at `start` milliseconds,
    /// spending its whole time running.
    fn sandboxed<'a>(output: &'a [&'a str], start: i64) -> Spawn<'a> {
        Spawn {
            start: Some(at(start)),
            phases: &[(Timed::Execute, SANDBOXED_RUN)],
            ..spawn("linux-sandbox", output, SANDBOXED_RUN)
        }
    }

    fn cache_hit<'a>(runner: &'a str, output: &'a [&'a str], total: u64) -> Spawn<'a> {
        Spawn {
            cache_hit: true,
            // The run that wrote the cache entry, long before the build.
            start: Some(EpochMicros::from_millis(0)),
            ..spawn(runner, output, total)
        }
    }

    fn only(attempts: &Attempts, output: &str) -> Attempt {
        let tried = attempts.action(output).expect("the action is held");
        assert_eq!(tried.len(), 1, "{output}");
        tried[0]
    }

    /// A spawn `runner` ran here, its subprocess started at 1 000
    /// milliseconds, set up for 20 and waiting 30 for resources before
    /// running for 110 in all.
    fn ran_here(runner: &str) -> Spawn<'_> {
        Spawn {
            phases: &[
                (Timed::Setup, 20),
                (Timed::Queue, 30),
                (Timed::Execute, 100),
                (Timed::ProcessOutputs, 10),
            ],
            start: Some(at(1_000)),
            ..spawn(runner, &["bazel-out/a"], 160)
        }
    }

    #[test]
    fn a_sandboxed_spawn_runs_while_its_sandbox_is_built_once_given_resources() {
        let dated = log(&[ran_here("linux-sandbox")])
            .dated()
            .expect("a spawn ran here");
        let a = only(&dated.attempts, "bazel-out/a");
        assert_eq!((a.start(), a.end()), (at(950), at(1_110)));
        assert_eq!(a.entered(Phase::Scheduling), Some(at(950)));
        assert_eq!(a.entered(LOCAL), Some(at(980)));
        assert_eq!(a.done(), Done::Executed);
        assert_eq!(
            dated.placement.span(),
            Interval::new(at(950), at(1_110)).expect("ordered")
        );
        assert!(!dated.placement.interpolated());
    }

    #[test]
    fn an_unsandboxed_spawn_is_scheduled_once_set_up() {
        let dated = log(&[ran_here("local")]).dated().expect("a spawn ran here");
        let a = only(&dated.attempts, "bazel-out/a");
        assert_eq!((a.start(), a.end()), (at(950), at(1_110)));
        assert_eq!(a.entered(Phase::Scheduling), Some(at(970)));
        assert_eq!(a.entered(LOCAL), Some(at(1_000)));
    }

    #[test]
    fn a_worker_logs_the_start_of_its_whole_spawn() {
        let dated = log(&[Spawn {
            phases: &[(Timed::Setup, 5), (Timed::Execute, 50)],
            start: Some(at(1_000)),
            ..spawn("worker", &["bazel-out/a"], 60)
        }])
        .dated()
        .expect("a spawn ran here");
        let a = only(&dated.attempts, "bazel-out/a");
        assert_eq!((a.start(), a.end()), (at(1_000), at(1_060)));
        assert_eq!(a.entered(Phase::Scheduling), Some(at(1_005)));
        assert_eq!(a.entered(LOCAL), Some(at(1_010)));
    }

    #[test]
    fn a_cache_hit_ends_between_its_neighbours_by_its_place_in_the_log() {
        let dated = log(&[
            sandboxed(&["bazel-out/a"], 1_000),
            cache_hit("disk cache hit", &["bazel-out/b"], 30),
            cache_hit("remote cache hit", &["bazel-out/c"], 40),
            sandboxed(&["bazel-out/d"], 1_300),
        ])
        .dated()
        .expect("spawns ran here");
        let b = only(&dated.attempts, "bazel-out/b");
        assert_eq!((b.start(), b.end()), (at(1_170), at(1_200)));
        assert_eq!(b.entered(Phase::CheckingCache), Some(at(1_170)));
        assert_eq!(
            b.state_at(at(1_200)),
            State::Done(Done::Cached(Cache::Spawn(SpawnCache::Disk)))
        );
        let c = only(&dated.attempts, "bazel-out/c");
        assert_eq!((c.start(), c.end()), (at(1_260), at(1_300)));
        assert_eq!(c.done(), Done::Cached(Cache::Spawn(SpawnCache::Remote)));
        assert!(dated.placement.interpolated());
    }

    #[test]
    fn spawns_beyond_the_placed_ones_move_on_at_their_mean_pace() {
        let dated = log(&[
            cache_hit("disk cache hit", &["bazel-out/a"], 10),
            sandboxed(&["bazel-out/b"], 1_000),
            sandboxed(&["bazel-out/c"], 1_200),
            cache_hit("disk cache hit", &["bazel-out/d"], 10),
        ])
        .dated()
        .expect("spawns ran here");
        assert_eq!(only(&dated.attempts, "bazel-out/a").end(), at(900));
        assert_eq!(only(&dated.attempts, "bazel-out/d").end(), at(1_500));
        assert_eq!(
            dated.placement.span(),
            Interval::new(at(890), at(1_500)).expect("ordered")
        );
    }

    #[test]
    fn spawns_beside_one_placed_spawn_end_with_it() {
        let dated = log(&[
            cache_hit("disk cache hit", &["bazel-out/a"], 10),
            sandboxed(&["bazel-out/b"], 1_000),
            cache_hit("disk cache hit", &["bazel-out/c"], 10),
        ])
        .dated()
        .expect("a spawn ran here");
        for output in ["bazel-out/a", "bazel-out/c"] {
            assert_eq!(only(&dated.attempts, output).end(), at(1_100));
        }
    }

    #[test]
    fn interpolated_ends_never_go_back_along_the_log() {
        let dated = log(&[
            sandboxed(&["bazel-out/a"], 2_000),
            cache_hit("disk cache hit", &["bazel-out/b"], 10),
            sandboxed(&["bazel-out/c"], 1_000),
            cache_hit("disk cache hit", &["bazel-out/d"], 10),
            sandboxed(&["bazel-out/e"], 2_200),
        ])
        .dated()
        .expect("spawns ran here");
        assert_eq!(only(&dated.attempts, "bazel-out/c").end(), at(1_100));
        assert_eq!(only(&dated.attempts, "bazel-out/b").end(), at(2_100));
        assert_eq!(only(&dated.attempts, "bazel-out/d").end(), at(2_200));
    }

    #[test]
    fn a_log_with_no_start_on_the_local_clock_cannot_be_dated() {
        let unstarted = Spawn {
            phases: &[(Timed::Execute, 10)],
            ..spawn("linux-sandbox", &["bazel-out/a"], 10)
        };
        let at_the_epoch = Spawn {
            start: Some(EpochMicros::from_millis(0)),
            ..spawn("local", &["bazel-out/b"], 10)
        };
        let remote = Spawn {
            start: Some(at(1_000)),
            ..spawn("remote", &["bazel-out/c"], 10)
        };
        let hit = cache_hit("disk cache hit", &["bazel-out/d"], 10);
        assert!(
            log(&[unstarted, at_the_epoch, remote, hit])
                .dated()
                .is_none()
        );
        assert!(log(&[]).dated().is_none());
    }

    #[test]
    fn a_remote_execution_checks_the_cache_queues_then_runs_remotely() {
        let dated = log(&[
            sandboxed(&["bazel-out/a"], 1_000),
            Spawn {
                phases: &[
                    (Timed::Parse, 5),
                    (Timed::Upload, 20),
                    (Timed::Queue, 30),
                    (Timed::Setup, 10),
                    (Timed::Execute, 100),
                    (Timed::ProcessOutputs, 15),
                    (Timed::Fetch, 20),
                ],
                ..spawn("remote", &["bazel-out/b"], 250)
            },
            sandboxed(&["bazel-out/c"], 1_400),
        ])
        .dated()
        .expect("spawns ran here");
        let b = only(&dated.attempts, "bazel-out/b");
        assert_eq!((b.start(), b.end()), (at(1_050), at(1_300)));
        assert_eq!(b.entered(Phase::CheckingCache), Some(at(1_055)));
        assert_eq!(b.entered(Phase::Scheduling), Some(at(1_125)));
        assert_eq!(b.entered(REMOTE), Some(at(1_155)));
        assert_eq!(b.state_at(at(1_290)), State::In(REMOTE));
        assert_eq!(b.done(), Done::Executed);
    }

    #[test]
    fn a_spawn_failing_ends_its_attempt_failed() {
        let dated = log(&[Spawn {
            exit_code: 1,
            ..sandboxed(&["bazel-out/a"], 1_000)
        }])
        .dated()
        .expect("a spawn ran here");
        assert_eq!(only(&dated.attempts, "bazel-out/a").done(), Done::Failed);
    }

    /// A spawn of the test `//t:t_test` writing `outputs`.
    fn test<'a>(outputs: &'a [&'a str], start: i64) -> Spawn<'a> {
        Spawn {
            mnemonic: TEST_MNEMONIC,
            target_label: "//t:t_test",
            ..sandboxed(outputs, start)
        }
    }

    #[test]
    fn a_test_s_spawns_are_attempts_of_the_action_writing_its_log() {
        let dated = log(&[
            test(
                &[
                    "bazel-out/k8/testlogs/t/t_test/test.xml",
                    "bazel-out/k8/testlogs/t/t_test/test.exited_prematurely",
                ],
                1_000,
            ),
            test(
                &[
                    "bazel-out/k8/testlogs/t/t_test/shard_1_of_2/test.xml",
                    "bazel-out/k8/testlogs/t/t_test/shard_1_of_2/test.exited_prematurely",
                ],
                1_000,
            ),
            test(
                &[
                    "bazel-out/k8/testlogs/t/t_test/test.xml",
                    "bazel-out/k8/testlogs/t/t_test/test.exited_prematurely",
                ],
                1_000,
            ),
        ])
        .dated()
        .expect("spawns ran here");
        let tried = |log| dated.attempts.action(log).map(<[Attempt]>::len);
        assert_eq!(tried("bazel-out/k8/testlogs/t/t_test/test.log"), Some(2));
        assert_eq!(
            tried("bazel-out/k8/testlogs/t/t_test/shard_1_of_2/test.log"),
            Some(1)
        );
        let owners: Vec<_> = dated
            .attempts
            .actions()
            .map(|(_, owner, _)| owner)
            .collect();
        assert!(
            owners
                .iter()
                .all(|&owner| owner == Some(ConfiguredTarget::new("//t:t_test", None)))
        );
    }

    #[test]
    fn a_spawn_following_a_test_s_run_carries_on_its_attempt() {
        let run = Spawn {
            exit_code: 1,
            ..test(
                &[
                    "bazel-out/k8/testlogs/t/t_test/test.xml",
                    "bazel-out/k8/testlogs/t/t_test/test.exited_prematurely",
                ],
                1_000,
            )
        };
        let report = test(&["bazel-out/k8/testlogs/t/t_test/test.xml"], 1_200);
        let dated = log(&[run, report]).dated().expect("spawns ran here");
        let a = only(&dated.attempts, "bazel-out/k8/testlogs/t/t_test/test.log");
        assert_eq!((a.start(), a.end()), (at(1_000), at(1_300)));
        assert_eq!(a.done(), Done::Failed);
    }

    #[test]
    fn a_spawn_naming_no_output_is_kept_by_its_target() {
        let dated = log(&[sandboxed(&[], 1_000)])
            .dated()
            .expect("a spawn ran here");
        let owned: Vec<_> = dated.attempts.by_owner().collect();
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0].0, ConfiguredTarget::new("//p:t", None));
    }
}
