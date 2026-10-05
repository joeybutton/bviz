//! The execution attempts a build event stream records of its build's
//! actions and tests.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::time::Duration;

use bviz_invocation::{
    Attempt, Attempts, Cache, ConfiguredTarget, Done, EpochMicros, Interval, SpawnCache, TEST_LOG,
};

use crate::bep::build_event_stream::{
    ActionExecuted, File, TestResult, TestStatus, build_event_id::Id, file,
};
use crate::outcomes::target;
use crate::output::{Began, Position, epoch_opt};
use crate::replay::Dating;

/// When a recorded attempt took place, as far as its stream tells.
#[derive(Debug, Clone, Copy, PartialEq)]
enum When {
    /// From `start` until `end`, both instants of this build.
    Took {
        start: EpochMicros,
        end: EpochMicros,
    },
    /// Ended where the event at this position stands: the stream gives the
    /// attempt no span, or one a cache recorded, in whole or in part, of
    /// another run.
    EndedAt(Position),
}

/// One attempt as its stream records it.
#[derive(Debug)]
struct Recorded {
    primary_output: Option<String>,
    owner: Option<ConfiguredTarget>,
    /// The run of its owner's test the attempt is one of; `None` for an
    /// action's attempt.
    test: Option<TestRun>,
    when: When,
    done: Done,
}

/// Which of a test's runs and which of its shards an attempt is one of: the
/// test's action that Bazel attempts again after a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TestRun {
    run: i32,
    shard: i32,
}

/// The attempts a stream records, in stream order.
#[derive(Debug, Default)]
pub struct RecordedAttempts(Vec<Recorded>);

impl RecordedAttempts {
    /// Records the attempt an `ActionExecuted` event at `position` reports,
    /// which a stream holds of a failed action and, under
    /// `--build_event_publish_all_actions`, of every action executed. Its
    /// span runs from the first of its spawns' starts to the last of their
    /// ends, which for a spawn a cache answered are those of the run the
    /// cache recorded, so only a span ending before the build began is one a
    /// cache answered whole:
    /// <https://github.com/bazelbuild/bazel/blob/9.1.0/src/main/java/com/google/devtools/build/lib/skyframe/SkyframeActionExecutor.java#L1904-L1931>
    pub(crate) fn action(
        &mut self,
        id: Option<&Id>,
        action: &ActionExecuted,
        began: Began,
        position: Position,
    ) {
        let Some(Id::ActionCompleted(id)) = id else {
            return;
        };
        let start = epoch_opt(action.start_time);
        let end = epoch_opt(action.end_time).or(start);
        let (when, answered) = match start.zip(end) {
            Some((start, end)) if began.took(Some(start)) => (
                When::Took {
                    start: EpochMicros::from_secs_f64(start),
                    end: EpochMicros::from_secs_f64(end),
                },
                Done::Executed,
            ),
            Some((_, end)) if !began.took(Some(end)) => (
                When::EndedAt(position),
                Done::Cached(Cache::Spawn(SpawnCache::RemoteOrDisk)),
            ),
            Some(_) | None => (When::EndedAt(position), Done::Executed),
        };
        self.0.push(Recorded {
            primary_output: Some(id.primary_output.clone()).filter(|output| !output.is_empty()),
            owner: (!id.label.is_empty())
                .then(|| target(id.label.clone(), id.configuration.clone())),
            test: None,
            when,
            done: match action.success {
                true => answered,
                false => Done::Failed,
            },
        });
    }

    /// Records the attempt of a test a `TestResult` event at `position`
    /// reports, as an attempt of the test's action, which its log names. A
    /// result a cache answered gives the times of the run it recorded.
    pub(crate) fn test(
        &mut self,
        id: Option<&Id>,
        result: &TestResult,
        began: Began,
        position: Position,
    ) {
        let Some(Id::TestResult(id)) = id else {
            return;
        };
        // A spawn cache's hit is told by the runner it names as the
        // strategy, whatever cache answered:
        // <https://github.com/bazelbuild/bazel/blob/9.1.0/src/main/java/com/google/devtools/build/lib/exec/StandaloneTestStrategy.java#L374-L381>
        let spawn_cache = result
            .execution_info
            .as_ref()
            .filter(|info| info.cached_remotely)
            .map(|info| SpawnCache::answering(&info.strategy));
        let answered = match (result.cached_locally, spawn_cache) {
            (true, _) => Done::Cached(Cache::Action),
            (false, Some(cache)) => Done::Cached(Cache::Spawn(cache)),
            (false, None) => Done::Executed,
        };
        let start = epoch_opt(result.test_attempt_start);
        let when = match start {
            Some(start) if answered == Done::Executed && began.took(Some(start)) => {
                let start = EpochMicros::from_secs_f64(start);
                let lasted = result
                    .test_attempt_duration
                    .map_or(Duration::ZERO, |lasted| {
                        Duration::new(
                            u64::try_from(lasted.seconds).unwrap_or(0),
                            u32::try_from(lasted.nanos).unwrap_or(0),
                        )
                    });
                When::Took {
                    start,
                    end: start.after(lasted),
                }
            }
            _ => When::EndedAt(position),
        };
        let failed = matches!(
            result.status(),
            TestStatus::Failed
                | TestStatus::Timeout
                | TestStatus::RemoteFailure
                | TestStatus::FailedToBuild
        );
        self.0.push(Recorded {
            primary_output: result.test_action_output.iter().find_map(test_log),
            owner: Some(target(id.label.clone(), id.configuration.clone())),
            test: Some(TestRun {
                run: id.run,
                shard: id.shard,
            }),
            when,
            done: match failed {
                true => Done::Failed,
                false => answered,
            },
        });
    }

    /// From the first start to the last end of the attempts the stream gives
    /// a span of this build's; `None` where it gives none.
    pub fn span(&self) -> Option<Interval> {
        self.0
            .iter()
            .filter_map(|recorded| match recorded.when {
                When::Took { start, end } => Interval::new(start, end.max(start)),
                When::EndedAt(_) => None,
            })
            .reduce(Interval::hull)
    }

    /// The attempts on the wall clock `dating` places the stream's events
    /// on. One the stream gives no span of this build's lasts no time, and
    /// ends where its event stands. The attempts of a run of a test whose log
    /// the stream does not name are pooled under the test as one, from the
    /// first's start to the last's end and ending as the last does, as the
    /// run ends: an attempt is retried only after a failure.
    pub fn dated(self, dating: Dating) -> Attempts {
        let mut attempts = Attempts::with_capacity(self.0.len());
        let mut runs: Vec<(ConfiguredTarget, Attempt)> = Vec::new();
        let mut run_at: HashMap<(ConfiguredTarget, TestRun), usize> = HashMap::new();
        for recorded in self.0 {
            let (start, end) = match recorded.when {
                When::Took { start, end } => (start, end),
                When::EndedAt(position) => {
                    let at = dating.instant_at(position);
                    (at, at)
                }
            };
            let attempt = Attempt::ended(start, end, [], recorded.done);
            match (&recorded.primary_output, recorded.owner, recorded.test) {
                (None, Some(owner), Some(test)) => match run_at.entry((owner, test)) {
                    Entry::Occupied(at) => {
                        let tried = &mut runs[*at.get()].1;
                        *tried = Attempt::ended(
                            tried.start().min(start),
                            tried.end().max(end),
                            [],
                            recorded.done,
                        );
                    }
                    Entry::Vacant(at) => {
                        runs.push((at.key().0.clone(), attempt));
                        at.insert(runs.len() - 1);
                    }
                },
                (primary_output, owner, _) => attempts.record(
                    primary_output.as_deref(),
                    owner.as_ref().map(ConfiguredTarget::borrowed),
                    attempt,
                ),
            }
        }
        for (owner, run) in &runs {
            attempts.record(None, Some(owner.borrowed()), *run);
        }
        attempts
    }
}

/// What Bazel adds to the name of a test's log to name the directory it
/// moves the logs of the attempts before the last into, and to the number of
/// such an attempt to name its log there:
/// <https://github.com/bazelbuild/bazel/blob/9.1.0/src/main/java/com/google/devtools/build/lib/exec/StandaloneTestStrategy.java#L153-L160>
const ATTEMPTS_DIRECTORY_SUFFIX: &str = "_attempts";
const ATTEMPT_LOG_PREFIX: &str = "attempt_";
const TEST_LOG_EXTENSION: &str = ".log";

/// The directory of an output base that holds the execution root, under
/// the name of the main repository.
const EXECROOT: &str = "/execroot/";

/// The exec path of the log of the test whose log `output` is, where it is
/// one on this machine: its URI names the file under the execution root.
/// The log of an attempt before the last lies apart from the test's, under a
/// name of its own, and stands for the test's:
/// <https://github.com/bazelbuild/bazel/blob/9.1.0/src/main/java/com/google/devtools/build/lib/analysis/test/TestAttempt.java#L263-L268>
fn test_log(output: &File) -> Option<String> {
    let Some(file::File::Uri(uri)) = &output.file else {
        return None;
    };
    if output.name != TEST_LOG {
        return None;
    }
    let (_, in_execroot) = uri.split_once(EXECROOT)?;
    let (_, exec_path) = in_execroot.split_once('/')?;
    Some(attempted_test_log(exec_path).unwrap_or_else(|| exec_path.to_owned()))
}

/// The exec path of the test's log, where `exec_path` is the log of an
/// attempt before its last.
fn attempted_test_log(exec_path: &str) -> Option<String> {
    let (directory, attempt) = exec_path.rsplit_once('/')?;
    let number = attempt
        .strip_prefix(ATTEMPT_LOG_PREFIX)?
        .strip_suffix(TEST_LOG_EXTENSION)?;
    if number.is_empty() || !number.bytes().all(|digit| digit.is_ascii_digit()) {
        return None;
    }
    let (test_directory, attempts) = directory.rsplit_once('/')?;
    let stem = attempts.strip_suffix(ATTEMPTS_DIRECTORY_SUFFIX)?;
    Some(format!("{test_directory}/{stem}{TEST_LOG_EXTENSION}"))
}

#[cfg(test)]
mod tests {
    use prost::Message;

    use super::*;
    use crate::bep::build_event_stream::{
        self as bes, BuildEvent, BuildEventId,
        build_event::Payload,
        build_event_id::{ActionCompletedId, ConfigurationId, TestResultId},
    };
    use crate::bep::duration_proto::google::protobuf::Duration as ProtoDuration;
    use crate::bep::timestamp_proto::google::protobuf::Timestamp;
    use crate::{Replay, parse};

    const BEGAN: i64 = 1_790_952_715;
    const CONFIG: &str = "a7a71fd16b14";

    fn at(seconds: i64) -> Option<Timestamp> {
        Some(Timestamp { seconds, nanos: 0 })
    }

    fn event(id: Option<Id>, payload: Payload) -> Vec<u8> {
        BuildEvent {
            id: id.map(|id| BuildEventId { id: Some(id) }),
            payload: Some(payload),
            ..Default::default()
        }
        .encode_length_delimited_to_vec()
    }

    fn progress() -> Vec<u8> {
        event(
            None,
            Payload::Progress(bes::Progress {
                stderr: "x".into(),
                ..Default::default()
            }),
        )
    }

    fn executed(primary_output: &str, executed: ActionExecuted) -> Vec<u8> {
        let id = Id::ActionCompleted(ActionCompletedId {
            primary_output: primary_output.into(),
            label: "//app:cli".into(),
            configuration: Some(ConfigurationId { id: CONFIG.into() }),
        });
        event(Some(id), Payload::Action(executed))
    }

    /// A stream started at [`BEGAN`] holding `events` after one progress
    /// frame, and finished after another.
    fn built(events: &[Vec<u8>]) -> Vec<u8> {
        let started = event(
            None,
            Payload::Started(bes::BuildStarted {
                start_time: at(BEGAN),
                ..Default::default()
            }),
        );
        let finished = event(
            None,
            Payload::Finished(bes::BuildFinished {
                finish_time: at(BEGAN + 10),
                ..Default::default()
            }),
        );
        [started, progress(), events.concat(), progress(), finished].concat()
    }

    fn instant(seconds: i64) -> EpochMicros {
        EpochMicros::from_millis(seconds * 1_000)
    }

    /// The attempts `stream` records, and where its replay dates each
    /// position.
    fn dated(stream: &[u8]) -> (Attempts, impl Fn(Position) -> EpochMicros) {
        let mut output = parse(stream).expect("parse stream");
        let recorded = std::mem::take(&mut output.attempts);
        let replay = Replay::from_output(output);
        let dating = replay.dating().expect("on the wall clock");
        let attempts = recorded.dated(dating);
        let instants: Vec<EpochMicros> = (0..=replay.frame_count())
            .map(|frame| dating.instant_at(Position(frame)))
            .collect();
        (attempts, move |position: Position| instants[position.0])
    }

    #[test]
    fn an_action_is_attempted_over_its_span_of_this_build_and_otherwise_where_it_ended() {
        let stream = built(&[
            executed(
                "bazel-out/bin/ran",
                ActionExecuted {
                    success: true,
                    start_time: at(BEGAN + 1),
                    end_time: at(BEGAN + 3),
                    ..Default::default()
                },
            ),
            progress(),
            executed(
                "bazel-out/bin/cached",
                ActionExecuted {
                    success: true,
                    start_time: at(BEGAN - 600),
                    end_time: at(BEGAN - 590),
                    ..Default::default()
                },
            ),
            executed(
                "bazel-out/bin/partly_cached",
                ActionExecuted {
                    success: true,
                    start_time: at(BEGAN - 600),
                    end_time: at(BEGAN + 2),
                    ..Default::default()
                },
            ),
            executed("bazel-out/bin/broken", ActionExecuted::default()),
        ]);
        let (attempts, dating) = dated(&stream);

        let ran = attempts.action("bazel-out/bin/ran").expect("ran");
        assert_eq!(
            ran,
            [Attempt::ended(
                instant(BEGAN + 1),
                instant(BEGAN + 3),
                [],
                Done::Executed
            )]
        );
        let ended = dating(Position(2));
        assert_eq!(
            attempts.action("bazel-out/bin/cached"),
            Some(
                &[Attempt::ended(
                    ended,
                    ended,
                    [],
                    Done::Cached(Cache::Spawn(SpawnCache::RemoteOrDisk))
                )][..]
            )
        );
        assert_eq!(
            attempts.action("bazel-out/bin/partly_cached"),
            Some(&[Attempt::ended(ended, ended, [], Done::Executed)][..])
        );
        assert_eq!(
            attempts.action("bazel-out/bin/broken"),
            Some(&[Attempt::ended(ended, ended, [], Done::Failed)][..])
        );
        let (_, owner, _) = (attempts.actions())
            .find(|(output, _, _)| *output == "bazel-out/bin/ran")
            .expect("held");
        assert_eq!(
            owner,
            Some(ConfiguredTarget::new("//app:cli", Some(CONFIG)))
        );
    }

    fn tested(label: &str, log: &str, result: TestResult) -> Vec<u8> {
        let id = Id::TestResult(TestResultId {
            label: label.into(),
            configuration: Some(ConfigurationId { id: CONFIG.into() }),
            run: 1,
            shard: 1,
            attempt: 1,
        });
        let uri = format!("file:///base/execroot/_main/{log}");
        let outputs = [("test.xml", format!("{uri}.xml")), (TEST_LOG, uri)];
        let result = TestResult {
            test_action_output: (outputs.into_iter())
                .map(|(name, uri)| File {
                    name: name.into(),
                    file: Some(file::File::Uri(uri)),
                    ..Default::default()
                })
                .collect(),
            ..result
        };
        event(Some(id), Payload::TestResult(result))
    }

    #[test]
    fn a_test_is_attempted_as_its_action_over_its_run_and_otherwise_where_it_ended() {
        const PASSES: &str = "bazel-out/k8-fastbuild/testlogs/t/passes/test.log";
        const FAILS: &str = "bazel-out/k8-fastbuild/testlogs/t/fails/shard_1_of_2/test.log";
        const CACHED: &str = "bazel-out/k8-fastbuild/testlogs/t/cached/test.log";
        let ran = |status: TestStatus| TestResult {
            status: status.into(),
            test_attempt_start: at(BEGAN + 4),
            test_attempt_duration: Some(ProtoDuration {
                seconds: 2,
                nanos: 500_000_000,
            }),
            ..Default::default()
        };
        let stream = built(&[
            tested("//t:passes", PASSES, ran(TestStatus::Passed)),
            tested("//t:fails", FAILS, ran(TestStatus::Failed)),
            tested(
                "//t:cached",
                CACHED,
                TestResult {
                    cached_locally: true,
                    test_attempt_start: at(BEGAN - 600),
                    ..ran(TestStatus::Passed)
                },
            ),
        ]);
        let (attempts, dating) = dated(&stream);
        let (start, end) = (
            instant(BEGAN + 4),
            instant(BEGAN + 4).plus_micros(2_500_000),
        );
        assert_eq!(
            attempts.action(PASSES),
            Some(&[Attempt::ended(start, end, [], Done::Executed)][..])
        );
        assert_eq!(
            attempts.action(FAILS),
            Some(&[Attempt::ended(start, end, [], Done::Failed)][..])
        );
        let ended = dating(Position(1));
        let up_to_date = Done::Cached(Cache::Action);
        assert_eq!(
            attempts.action(CACHED),
            Some(&[Attempt::ended(ended, ended, [], up_to_date)][..])
        );
        let (_, owner, _) = (attempts.actions())
            .find(|(output, _, _)| *output == PASSES)
            .expect("held");
        assert_eq!(
            owner,
            Some(ConfiguredTarget::new("//t:passes", Some(CONFIG)))
        );
    }

    #[test]
    fn a_test_s_cached_attempt_names_the_cache_its_strategy_names() {
        const LOG: &str = "bazel-out/k8-fastbuild/testlogs/t/cached/test.log";
        let hit = |strategy: &str| TestResult {
            status: TestStatus::Passed.into(),
            execution_info: Some(bes::test_result::ExecutionInfo {
                cached_remotely: true,
                strategy: strategy.into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        for (strategy, cache) in [
            ("disk cache hit", SpawnCache::Disk),
            ("remote cache hit", SpawnCache::Remote),
            ("", SpawnCache::RemoteOrDisk),
        ] {
            let (attempts, _) = dated(&built(&[tested("//t:cached", LOG, hit(strategy))]));
            let tried = attempts.action(LOG).expect("held");
            assert_eq!(
                tried[0].done(),
                Done::Cached(Cache::Spawn(cache)),
                "{strategy}"
            );
        }
    }

    #[test]
    fn a_run_of_a_test_without_a_log_on_this_machine_ends_as_its_last_attempt() {
        let attempted = |shard: i32, attempt: i32, status: TestStatus, start: i64| {
            let id = Id::TestResult(TestResultId {
                label: "//t:flaky".into(),
                configuration: Some(ConfigurationId { id: CONFIG.into() }),
                run: 1,
                shard,
                attempt,
            });
            let log = File {
                name: TEST_LOG.into(),
                file: Some(file::File::Uri(
                    "bytestream://cache/blobs/9f86d081/1".into(),
                )),
                ..Default::default()
            };
            let result = TestResult {
                status: status.into(),
                test_attempt_start: at(start),
                test_attempt_duration: Some(ProtoDuration {
                    seconds: 1,
                    nanos: 0,
                }),
                test_action_output: vec![log],
                ..Default::default()
            };
            event(Some(id), Payload::TestResult(result))
        };
        let stream = built(&[
            attempted(1, 1, TestStatus::Failed, BEGAN + 1),
            attempted(2, 1, TestStatus::Failed, BEGAN + 2),
            attempted(1, 2, TestStatus::Passed, BEGAN + 3),
        ]);
        let (attempts, _) = dated(&stream);
        let owner = ConfiguredTarget::new("//t:flaky", Some(CONFIG));
        let pooled: Vec<_> = attempts.by_owner().collect();
        assert_eq!(pooled.len(), 1);
        assert_eq!(pooled[0].0, owner);
        let runs: Vec<_> = pooled[0]
            .1
            .iter()
            .map(|run| (run.start(), run.end(), run.done()))
            .collect();
        let after = |seconds: i64| instant(BEGAN + seconds);
        assert_eq!(
            runs,
            [
                (after(1), after(4), Done::Executed),
                (after(2), after(3), Done::Failed)
            ]
        );
    }

    #[test]
    fn a_test_log_is_found_only_under_the_execution_root() {
        let log = |name: &str, uri: &str| {
            test_log(&File {
                name: name.into(),
                file: Some(file::File::Uri(uri.into())),
                ..Default::default()
            })
        };
        assert_eq!(
            log(
                TEST_LOG,
                "file:///b/execroot/_main/bazel-out/k8/testlogs/t/test.log"
            )
            .as_deref(),
            Some("bazel-out/k8/testlogs/t/test.log")
        );
        assert_eq!(
            log(
                TEST_LOG,
                "file:///b/execroot/_main/bazel-out/k8/testlogs/t/test_attempts/attempt_1.log"
            )
            .as_deref(),
            Some("bazel-out/k8/testlogs/t/test.log")
        );
        assert_eq!(
            log(
                TEST_LOG,
                "file:///b/execroot/_main/bazel-out/k8/testlogs/t/test_attempts/x.log"
            )
            .as_deref(),
            Some("bazel-out/k8/testlogs/t/test_attempts/x.log")
        );
        assert_eq!(log(TEST_LOG, "bytestream://cache/blobs/a1b2/42"), None);
        assert_eq!(log("test.xml", "file:///b/execroot/_main/t/test.xml"), None);
    }
}
