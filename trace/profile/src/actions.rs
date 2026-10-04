use std::time::Duration;

use bviz_invocation::{
    Attempt, Attempts, ConfiguredTarget, Phase, Profiled, Runner, Spawns, Timings, Venue,
};

use crate::model::{CategoryId, LaneClass, OptStr, Profile, Slice, Str, TimeSpan};

/// One action span the profile records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Action {
    /// What Bazel's `describe()` says the action does, the span's name.
    pub description: Str,
    pub mnemonic: OptStr,
    /// The action's primary output (see [`crate::SliceDetail::out`]).
    pub out: OptStr,
    pub target: OptStr,
    pub configuration: OptStr,
    pub span: TimeSpan,
    pub spawns: Spawns,
    pub phases: Sightings,
}

impl Action {
    pub fn wall(&self) -> Duration {
        micros(self.span.end_us.saturating_sub(self.span.start_us))
    }
}

/// When the spans nested in an action's span first showed each phase, in the
/// profile's microseconds. Bazel never records an action entering
/// [`Phase::Preparing`], which it does at the start of its span.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sightings([Option<i64>; Phase::ALL.len()]);

impl Sightings {
    pub fn of(&self, phase: Phase) -> Option<i64> {
        self.0[phase.index()]
    }

    /// Each phase seen, with when.
    pub fn seen(&self) -> impl Iterator<Item = (Phase, i64)> + '_ {
        Phase::ALL
            .into_iter()
            .filter_map(|phase| Some((phase, self.of(phase)?)))
    }

    fn is_empty(&self) -> bool {
        self.0.iter().all(Option::is_none)
    }

    fn saw(&mut self, phase: Phase, at_us: i64) {
        self.0[phase.index()].get_or_insert(at_us);
    }
}

/// The Bazel `cat` of the span that covers an action's execution from start to
/// finish, `ProfilerTask.ACTION`.
const ACTION_CATEGORY: &str = "action processing";

/// The span Bazel opens on the command's thread while it connects to a gRPC
/// remote cache or executor:
/// <https://github.com/bazelbuild/bazel/blob/9.1.0/src/main/java/com/google/devtools/build/lib/remote/RemoteModule.java#L653>
const REMOTE_CHANNEL_SPAN: &str = "init channel and check server capabilities";

/// The span a sandboxed spawn runs its subprocess under:
/// <https://github.com/bazelbuild/bazel/blob/9.1.0/src/main/java/com/google/devtools/build/lib/sandbox/AbstractSandboxSpawnRunner.java#L151>
const SANDBOXED_SUBPROCESS_SPAN: &str = "subprocess.run";

/// The span a sandboxed spawn opens once its subprocess has exited. Bazel
/// records it however briefly it lasts, so it shows a run whose own span fell
/// under the profile's threshold:
/// <https://github.com/bazelbuild/bazel/blob/9.1.0/src/main/java/com/google/devtools/build/lib/sandbox/AbstractSandboxSpawnRunner.java#L154>
const SANDBOX_POSTCONDITION_SPAN: &str = "sandbox.verifyPostCondition";

/// How the name of an `action resource lock` span begins when it waits for
/// local resources; Bazel records other waits under the category too:
/// <https://github.com/bazelbuild/bazel/blob/9.1.0/src/main/java/com/google/devtools/build/lib/actions/ResourceManager.java#L382>
/// <https://github.com/bazelbuild/bazel/blob/9.1.0/src/main/java/com/google/devtools/build/lib/remote/RemoteRewoundActionSynchronizer.java#L141>
const RESOURCE_WAIT_PREFIX: &str = "Acquiring resources for: ";

/// What one span nested in an action's span says of the action.
#[derive(Debug, Clone, Copy)]
struct Sign {
    runner: Option<Runner>,
    phase: Option<Phase>,
}

impl Sign {
    const SILENT: Sign = Sign {
        runner: None,
        phase: None,
    };

    const fn ran(runner: Runner, phase: Phase) -> Sign {
        Sign {
            runner: Some(runner),
            phase: Some(phase),
        }
    }

    const fn held(runner: Runner) -> Sign {
        Sign {
            runner: Some(runner),
            phase: None,
        }
    }

    const fn shows(phase: Phase) -> Sign {
        Sign {
            runner: None,
            phase: Some(phase),
        }
    }
}

/// What the spans of one Bazel `cat` nested in an action's span say of it.
#[derive(Debug, Clone, Copy)]
enum Evidence {
    Of(Sign),
    /// Left to the span's name, the category holding spans that say different
    /// things.
    Named(Shared),
}

/// A category whose spans [`Evidence::Named`] reads by name.
#[derive(Debug, Clone, Copy)]
enum Shared {
    Info,
    ResourceLock,
}

impl Evidence {
    fn of(category: &str, cache_hit: Runner) -> Evidence {
        use Runner::*;
        const LOCAL: Phase = Phase::Running(Venue::Local);
        const REMOTE: Phase = Phase::Running(Venue::Remote);
        Evidence::Of(match category {
            "remote action execution" | "Remote execution process wall time" => {
                Sign::ran(RemoteExecution, REMOTE)
            }
            // Also waiting to build the remote action, which comes before the
            // cache check, so it shows no phase:
            // <https://github.com/bazelbuild/bazel/blob/9.1.0/src/main/java/com/google/devtools/build/lib/remote/RemoteExecutionService.java#L507>
            "Remote execution setup" => Sign::held(RemoteExecution),
            "Remote execution queuing time" => Sign::ran(RemoteExecution, Phase::Scheduling),
            "local execution in worker" | "waiting for response from worker" => {
                Sign::ran(Worker, LOCAL)
            }
            "setting up inputs for worker"
            | "borrowing a worker"
            | "copying outputs from worker" => Sign::held(Worker),
            "local action execution" | "Local execution process wall time" => {
                Sign::ran(Local, LOCAL)
            }
            "Sandbox execution process wall time" => Sign::ran(Sandboxed, LOCAL),
            "remote action cache check" => Sign::ran(cache_hit, Phase::CheckingCache),
            "action resource lock" => return Evidence::Named(Shared::ResourceLock),
            "general information" => return Evidence::Named(Shared::Info),
            _ => Sign::SILENT,
        })
    }

    fn sign(self, profile: &Profile, slice: &Slice) -> Sign {
        let name = || profile.str(slice.name);
        match self {
            Evidence::Of(sign) => sign,
            Evidence::Named(Shared::Info) => match name() {
                SANDBOXED_SUBPROCESS_SPAN => {
                    Sign::ran(Runner::Sandboxed, Phase::Running(Venue::Local))
                }
                SANDBOX_POSTCONDITION_SPAN => Sign::shows(Phase::Running(Venue::Local)),
                _ => Sign::SILENT,
            },
            Evidence::Named(Shared::ResourceLock) if name().starts_with(RESOURCE_WAIT_PREFIX) => {
                Sign::shows(Phase::Scheduling)
            }
            Evidence::Named(Shared::ResourceLock) => Sign::SILENT,
        }
    }
}

/// Defers running entered in a span that encloses `scheduling` until
/// `scheduling` ends: a spawn runner opens its span before it waits for local
/// resources or in a remote executor's queue, and runs once the wait ends.
fn defer_running(
    phases: &mut Sightings,
    shown_until: &[i64; Phase::ALL.len()],
    scheduling: &Slice,
) {
    for venue in Venue::ALL {
        let running = Phase::Running(venue);
        if let Some(entered) = phases.of(running)
            && scheduling.start_us < shown_until[running.index()]
        {
            phases.0[running.index()] = Some(entered.max(scheduling.end_us()));
        }
    }
}

fn micros(dur_us: i64) -> Duration {
    Duration::from_micros(u64::try_from(dur_us).unwrap_or_default())
}

impl Profile {
    /// Every `action processing` span, in the order they started; one action
    /// executed more than once has a span for each execution.
    pub fn actions(&self) -> Vec<Action> {
        let Some(action_category) = self
            .categories()
            .position(|category| self.str(category) == ACTION_CATEGORY)
            .and_then(CategoryId::at)
        else {
            return Vec::new();
        };
        let cache_hit = self.cache_hit_runner();
        let evidence: Vec<Evidence> = self
            .categories()
            .map(|category| Evidence::of(self.str(category), cache_hit))
            .collect();
        let sign_of = |slice: &Slice| {
            slice
                .category
                .map_or(Sign::SILENT, |id| evidence[id.position()].sign(self, slice))
        };
        let is_action = |slice: &Slice| slice.category == Some(action_category);
        let span_count = self
            .threads()
            .iter()
            .map(|track| self.slices(track).iter().filter(|s| is_action(s)).count())
            .sum();
        let mut started = Vec::with_capacity(span_count);
        for track in self.threads() {
            let slices = self.slices(track);
            for (i, (slice, detail)) in slices.iter().zip(self.details(track)).enumerate() {
                if !is_action(slice) {
                    continue;
                }
                let mut spawns = Spawns::default();
                let mut phases = Sightings::default();
                // By runner, when the last span counted for it ends: a span of
                // the runner starting before then is nested in that one, whose
                // time covers its own.
                let mut counted_until = [i64::MIN; Runner::ALL.len()];
                // By phase, when the span that first showed it ends.
                let mut shown_until = [i64::MIN; Phase::ALL.len()];
                // A span's descendants are the run after it nested deeper.
                for nested in slices[i + 1..]
                    .iter()
                    .take_while(|nested| nested.depth > slice.depth)
                {
                    let sign = sign_of(nested);
                    if let Some(runner) = sign.runner {
                        let until = &mut counted_until[runner.index()];
                        if nested.start_us >= *until {
                            *until = nested.end_us();
                            spawns.ran(runner, micros(nested.dur_us));
                        }
                    }
                    match sign.phase {
                        Some(Phase::Scheduling) => {
                            phases.saw(Phase::Scheduling, nested.start_us);
                            defer_running(&mut phases, &shown_until, nested);
                        }
                        Some(phase) if phases.of(phase).is_none() => {
                            phases.saw(phase, nested.start_us);
                            shown_until[phase.index()] = nested.end_us();
                        }
                        _ => {}
                    }
                }
                if phases.is_empty() {
                    phases.saw(Phase::Running(Venue::Local), slice.start_us);
                }
                started.push(Action {
                    description: slice.name,
                    mnemonic: detail.mnemonic,
                    out: detail.out,
                    target: detail.target,
                    configuration: detail.configuration,
                    span: TimeSpan {
                        start_us: slice.start_us,
                        end_us: slice.end_us(),
                    },
                    spawns,
                    phases,
                });
            }
        }
        started.sort_by_key(|action| action.span.start_us);
        started
    }

    /// The time each of [its actions](Self::actions) took and where it ran.
    pub fn timings(&self) -> Timings {
        let actions = self.actions();
        let mut timings = Timings::with_capacity(actions.len());
        for action in &actions {
            timings.record(Profiled {
                primary_output: self.opt_str(action.out),
                owner: self.owner(action),
                wall: action.wall(),
                spawns: action.spawns,
            });
        }
        timings
    }

    /// Each of [its actions](Self::actions) as an execution attempt on the
    /// wall clock; `None` for a profile that does not say when its clock
    /// started.
    pub fn attempts(&self) -> Option<Attempts> {
        let origin = self.metadata().epoch_origin?;
        let actions = self.actions();
        let mut attempts = Attempts::with_capacity(actions.len());
        for action in &actions {
            let attempt = Attempt::new(
                origin.plus_micros(action.span.start_us),
                origin.plus_micros(action.span.end_us),
                action
                    .phases
                    .seen()
                    .map(|(phase, at_us)| (phase, origin.plus_micros(at_us))),
            );
            attempts.record(self.opt_str(action.out), self.owner(action), attempt);
        }
        Some(attempts)
    }

    fn owner(&self, action: &Action) -> Option<ConfiguredTarget<&str>> {
        self.opt_str(action.target)
            .map(|label| ConfiguredTarget::new(label, self.opt_str(action.configuration)))
    }

    /// The runner of an action answered by a cache lookup. Bazel records a
    /// lookup in the disk cache and in a remote one as the same span:
    /// <https://github.com/bazelbuild/bazel/blob/9.1.0/src/main/java/com/google/devtools/build/lib/remote/RemoteSpawnCache.java#L144-L147>
    /// so which cache answered is told only by whether the build connected to
    /// a remote endpoint. A hit in a disk cache that backs a gRPC remote one
    /// therefore reads as remote, and a hit in an HTTP remote cache, which
    /// opens no channel, as disk.
    fn cache_hit_runner(&self) -> Runner {
        let remote = self
            .threads()
            .iter()
            .filter(|track| track.lane_class == LaneClass::Main)
            .flat_map(|track| self.slices(track))
            .any(|slice| self.str(slice.name) == REMOTE_CHANNEL_SPAN);
        if remote {
            Runner::RemoteCacheHit
        } else {
            Runner::DiskCacheHit
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_invocation::{Done, EpochMicros, State, Timing};

    const ACTION_TIDS: [i64; 2] = [2, 3];
    const MAIN_TID: i64 = 1;
    /// When the clock of every profile [`profile`] writes started, in
    /// milliseconds since the Unix epoch.
    const ORIGIN_MS: i64 = 1_781_439_245_231;

    /// A span on an action thread: `(cat, name, ts, dur)`.
    type Span<'a> = (&'a str, &'a str, i64, i64);

    /// A profile whose action threads take turns to hold one action span
    /// lasting 100µs per entry of `actions`, writing the entry's primary output
    /// if it has one and followed by its spans, timed from the action's start.
    /// The main thread connects to a remote endpoint when `remote` is set.
    fn profile(remote: bool, actions: &[(Option<&str>, &[Span])]) -> Profile {
        let mut events = vec![format!(
            r#"{{"name":"thread_name","ph":"M","tid":{MAIN_TID},"args":{{"name":"Main Thread"}}}}"#
        )];
        for (i, tid) in ACTION_TIDS.into_iter().enumerate() {
            events.push(format!(
                r#"{{"name":"thread_name","ph":"M","tid":{tid},"args":{{"name":"skyframe-evaluator-execution-{i}"}}}}"#
            ));
        }
        if remote {
            events.push(format!(
                r#"{{"cat":"general information","name":"{REMOTE_CHANNEL_SPAN}","ph":"X","ts":0,"dur":1,"tid":{MAIN_TID}}}"#
            ));
        }
        let mut at = 0;
        for ((out, nested), tid) in actions.iter().zip(ACTION_TIDS.into_iter().cycle()) {
            let out = out.map_or(String::new(), |out| format!(r#","out":"{out}""#));
            events.push(format!(
                r#"{{"cat":"{ACTION_CATEGORY}","name":"a","ph":"X","ts":{at},"dur":100,"tid":{tid}{out},"args":{{"target":"//p:t","configuration":"c","mnemonic":"M"}}}}"#
            ));
            for (cat, name, ts, dur) in *nested {
                events.push(format!(
                    r#"{{"cat":"{cat}","name":"{name}","ph":"X","ts":{},"dur":{dur},"tid":{tid}}}"#,
                    at + ts
                ));
            }
            at += 1000;
        }
        let json = format!(
            r#"{{"otherData":{{"profile_start_ts":{ORIGIN_MS}}},"traceEvents":[{}]}}"#,
            events.join(",")
        );
        Profile::from_bytes(json.as_bytes()).expect("parse")
    }

    fn runner(remote: bool, nested: &[Span]) -> Runner {
        let actions = profile(remote, &[(Some("bazel-out/o"), nested)]).actions();
        assert_eq!(actions.len(), 1);
        actions[0].spawns.runner()
    }

    const CACHE_CHECK: Span = ("remote action cache check", "check cache hit", 1, 5);
    const ACTION_EXECUTE: Span = ("general information", "Action.execute", 1, 90);

    #[test]
    fn a_remote_execution_is_remote() {
        let nested = [
            ACTION_EXECUTE,
            CACHE_CHECK,
            ("remote action execution", "execute remotely", 10, 50),
            ("remote action execution", "send Execute request", 11, 40),
        ];
        assert_eq!(runner(true, &nested), Runner::RemoteExecution);
    }

    #[test]
    fn a_hit_is_a_remote_cache_hit_in_a_build_connected_to_a_remote_endpoint() {
        let nested = [
            ACTION_EXECUTE,
            CACHE_CHECK,
            ("remote output download", "download outputs", 10, 5),
        ];
        assert_eq!(runner(true, &nested), Runner::RemoteCacheHit);
        assert_eq!(runner(false, &nested), Runner::DiskCacheHit);
    }

    #[test]
    fn a_worker_spawn_is_a_worker() {
        let nested = [
            ACTION_EXECUTE,
            (
                "setting up inputs for worker",
                "Worker #1 preparing execution",
                2,
                3,
            ),
            (
                "waiting for response from worker",
                "Worker #1 working",
                10,
                50,
            ),
        ];
        assert_eq!(runner(false, &nested), Runner::Worker);
    }

    #[test]
    fn a_subprocess_in_a_sandbox_is_sandboxed_even_after_a_cache_miss() {
        let nested = [
            ACTION_EXECUTE,
            CACHE_CHECK,
            ("general information", "sandbox.createFileSystem", 8, 2),
            ("general information", "subprocess.run", 10, 50),
        ];
        assert_eq!(runner(true, &nested), Runner::Sandboxed);
    }

    #[test]
    fn a_sandboxed_process_is_sandboxed_and_its_subprocess_adds_no_time() {
        let nested = [
            ACTION_EXECUTE,
            ("Sandbox execution process wall time", "Genrule", 10, 60),
            ("general information", "subprocess.run", 11, 58),
        ];
        let actions = profile(false, &[(Some("bazel-out/o"), &nested)]).actions();
        let mut spawns = Spawns::default();
        spawns.ran(Runner::Sandboxed, Duration::from_micros(60));
        assert_eq!(actions[0].spawns, spawns);
    }

    #[test]
    fn an_unsandboxed_subprocess_is_local() {
        let nested = [
            ACTION_EXECUTE,
            ("local action execution", "Genrule", 10, 50),
            ("Local execution process wall time", "Genrule", 11, 48),
        ];
        assert_eq!(runner(false, &nested), Runner::Local);
    }

    #[test]
    fn an_action_with_no_spawn_on_its_thread_records_none() {
        let nested = [
            ACTION_EXECUTE,
            ("general information", "checkOutputs", 95, 2),
        ];
        assert_eq!(runner(true, &nested), Runner::NoSpawnRecorded);
    }

    #[test]
    fn a_long_local_run_falling_back_from_a_remote_attempt_that_failed_fast_is_local() {
        let nested = [
            ACTION_EXECUTE,
            CACHE_CHECK,
            ("remote action execution", "execute remotely", 10, 5),
            ("local action execution", "TestRunner", 20, 70),
        ];
        assert_eq!(runner(true, &nested), Runner::Local);
    }

    #[test]
    fn a_long_remote_run_followed_by_a_short_local_spawn_is_remote() {
        let nested = [
            ACTION_EXECUTE,
            CACHE_CHECK,
            ("remote action execution", "execute remotely", 10, 70),
            ("local action execution", "TestRunner", 85, 5),
        ];
        assert_eq!(runner(true, &nested), Runner::RemoteExecution);
    }

    #[test]
    fn a_span_nested_in_one_of_the_same_runner_adds_no_time_to_it() {
        let nested = [
            ACTION_EXECUTE,
            ("remote action execution", "execute remotely", 10, 30),
            ("remote action execution", "send Execute request", 11, 28),
            ("local action execution", "TestRunner", 50, 40),
        ];
        assert_eq!(runner(true, &nested), Runner::Local);
    }

    #[test]
    fn only_spans_nested_in_the_action_say_what_ran_it() {
        // Starts inside the action but outlives it, so it is the action's
        // sibling rather than its callee.
        let overlapping = ("local action execution", "Genrule", 50, 100);
        assert_eq!(runner(false, &[overlapping]), Runner::NoSpawnRecorded);
    }

    #[test]
    fn each_span_is_an_action_in_the_order_they_started_across_threads() {
        let sandboxed: &[Span] = &[("general information", "subprocess.run", 10, 50)];
        let hit: &[Span] = &[CACHE_CHECK];
        let p = profile(
            false,
            &[
                (Some("bazel-out/o"), hit),
                (None, hit),
                (Some("bazel-out/o"), sandboxed),
            ],
        );
        let actions = p.actions();
        let read: Vec<_> = actions
            .iter()
            .map(|a| (p.opt_str(a.out), a.spawns.runner()))
            .collect();
        assert_eq!(
            read,
            [
                (Some("bazel-out/o"), Runner::DiskCacheHit),
                (None, Runner::DiskCacheHit),
                (Some("bazel-out/o"), Runner::Sandboxed),
            ]
        );
        let a = &actions[0];
        assert_eq!(
            (p.opt_str(a.target), p.opt_str(a.configuration), a.wall()),
            (Some("//p:t"), Some("c"), Duration::from_micros(100))
        );
        assert_eq!(
            (p.str(a.description), p.opt_str(a.mnemonic)),
            ("a", Some("M"))
        );
    }

    #[test]
    fn timings_know_an_action_by_its_output_and_otherwise_by_its_target() {
        let hit: &[Span] = &[CACHE_CHECK];
        let p = profile(
            false,
            &[(Some("bazel-out/o"), hit), (None, hit), (None, hit)],
        );
        let timings = p.timings();
        let hit = Timing {
            wall: Duration::from_micros(100),
            runner: Runner::DiskCacheHit,
        };
        assert_eq!(timings.action("bazel-out/o"), Some(hit));
        let owned = timings
            .owned_by(ConfiguredTarget::new("//p:t", Some("c")))
            .expect("owned");
        assert_eq!(owned.of(Runner::DiskCacheHit).actions, 2);
        assert_eq!(owned.total().wall, Duration::from_micros(200));
        assert!(timings.names_primary_outputs() && timings.names_owners());
    }

    #[test]
    fn spans_sharing_a_primary_output_are_one_timing_run_where_their_executions_held_longest() {
        let mostly_sandboxed: &[Span] = &[
            ("general information", "subprocess.run", 10, 40),
            ("remote action execution", "execute remotely", 60, 10),
        ];
        let remote: &[Span] = &[("remote action execution", "execute remotely", 10, 35)];
        let p = profile(
            true,
            &[
                (Some("bazel-out/o"), mostly_sandboxed),
                (Some("bazel-out/o"), remote),
            ],
        );
        let runners: Vec<_> = p.actions().iter().map(|a| a.spawns.runner()).collect();
        assert_eq!(runners, [Runner::Sandboxed, Runner::RemoteExecution]);
        assert_eq!(
            p.timings().action("bazel-out/o"),
            Some(Timing {
                wall: Duration::from_micros(200),
                runner: Runner::RemoteExecution,
            })
        );
    }

    #[test]
    fn a_profile_without_actions_has_none() {
        assert!(profile(true, &[]).actions().is_empty());
    }

    const LOCAL: Phase = Phase::Running(Venue::Local);
    const REMOTE: Phase = Phase::Running(Venue::Remote);

    /// The instant `micros` into the clock of a profile [`profile`] writes.
    fn at(micros: i64) -> EpochMicros {
        EpochMicros::from_millis(ORIGIN_MS).plus_micros(micros)
    }

    /// The attempts at the actions of a profile [`profile`] writes, one per
    /// entry of `actions` and all writing one primary output.
    fn attempts(remote: bool, actions: &[&[Span]]) -> Vec<Attempt> {
        let named: Vec<_> = actions.iter().map(|&n| (Some("bazel-out/o"), n)).collect();
        let attempts = profile(remote, &named).attempts().expect("aligned");
        attempts.action("bazel-out/o").expect("recorded").to_vec()
    }

    fn attempt(remote: bool, nested: &[Span]) -> Attempt {
        let attempts = attempts(remote, &[nested]);
        assert_eq!(attempts.len(), 1);
        attempts[0]
    }

    /// When `attempt` entered each phase it did, in the profile's clock.
    fn entered(attempt: &Attempt) -> Vec<(Phase, i64)> {
        Phase::ALL
            .into_iter()
            .filter_map(|phase| {
                let entered = attempt.entered(phase)?;
                Some((phase, entered.micros() - at(0).micros()))
            })
            .collect()
    }

    #[test]
    fn an_attempt_spans_its_action_on_the_wall_clock() {
        let a = attempt(false, &[CACHE_CHECK]);
        assert_eq!((a.start(), a.end()), (at(0), at(100)));
    }

    #[test]
    fn a_profile_without_an_epoch_origin_has_no_attempts() {
        let json = format!(
            r#"{{"traceEvents":[{{"cat":"{ACTION_CATEGORY}","name":"a","ph":"X","ts":0,"dur":100,"tid":2}}]}}"#
        );
        let p = Profile::from_bytes(json.as_bytes()).expect("parse");
        assert_eq!(p.actions().len(), 1);
        assert!(p.attempts().is_none());
    }

    #[test]
    fn an_attempt_answered_by_a_cache_check_alone_is_done_cached() {
        let a = attempt(true, &[ACTION_EXECUTE, CACHE_CHECK]);
        assert_eq!(
            entered(&a),
            [(Phase::Preparing, 0), (Phase::CheckingCache, 1)]
        );
        assert_eq!(a.state_at(at(3)), State::In(Phase::CheckingCache));
        assert_eq!(a.state_at(at(50)), State::In(Phase::CheckingCache));
        assert_eq!(a.state_at(at(100)), State::Done(Done::Cached));
    }

    #[test]
    fn an_action_with_no_spawn_runs_locally_for_its_whole_span() {
        let a = attempt(
            true,
            &[
                ACTION_EXECUTE,
                ("general information", "checkOutputs", 95, 2),
            ],
        );
        assert_eq!(entered(&a), [(Phase::Preparing, 0), (LOCAL, 0)]);
        assert_eq!(a.state_at(at(0)), State::In(LOCAL));
        assert_eq!(a.state_at(at(100)), State::Done(Done::Executed));
    }

    #[test]
    fn a_local_spawn_schedules_while_it_waits_for_resources_then_runs() {
        let a = attempt(
            false,
            &[
                ACTION_EXECUTE,
                CACHE_CHECK,
                ("local action execution", "Genrule", 10, 80),
                (
                    "action resource lock",
                    "Acquiring resources for: Genrule",
                    11,
                    19,
                ),
                ("Local execution process wall time", "Genrule", 31, 58),
            ],
        );
        assert_eq!(
            entered(&a),
            [
                (Phase::Preparing, 0),
                (Phase::CheckingCache, 1),
                (Phase::Scheduling, 11),
                (LOCAL, 30),
            ]
        );
        assert_eq!(a.state_at(at(20)), State::In(Phase::Scheduling));
        assert_eq!(a.state_at(at(100)), State::Done(Done::Executed));
    }

    #[test]
    fn a_sandboxed_spawn_runs_locally_once_it_has_its_resources() {
        let a = attempt(
            false,
            &[
                ACTION_EXECUTE,
                (
                    "action resource lock",
                    "Acquiring resources for: Genrule",
                    5,
                    15,
                ),
                ("general information", "subprocess.run", 30, 50),
            ],
        );
        assert_eq!(
            entered(&a),
            [(Phase::Preparing, 0), (Phase::Scheduling, 5), (LOCAL, 30)]
        );
    }

    #[test]
    fn a_remote_execution_schedules_while_queued_then_runs_remotely() {
        let a = attempt(
            true,
            &[
                ACTION_EXECUTE,
                CACHE_CHECK,
                (
                    "Remote execution upload time",
                    "upload missing inputs",
                    7,
                    2,
                ),
                ("remote action execution", "execute remotely", 10, 80),
                ("remote action execution", "send Execute request", 11, 78),
                ("Remote execution queuing time", "queue", 12, 20),
                ("Remote execution process wall time", "execute", 35, 50),
            ],
        );
        assert_eq!(
            entered(&a),
            [
                (Phase::Preparing, 0),
                (Phase::CheckingCache, 1),
                (Phase::Scheduling, 12),
                (REMOTE, 32),
            ]
        );
        assert_eq!(a.state_at(at(9)), State::In(Phase::CheckingCache));
        assert_eq!(a.state_at(at(20)), State::In(Phase::Scheduling));
        assert_eq!(a.state_at(at(40)), State::In(REMOTE));
    }

    const CACHE_CHECK_LATE: Span = ("remote action cache check", "check cache hit", 20, 5);

    #[test]
    fn phases_only_move_forward() {
        let a = attempt(
            false,
            &[
                (
                    "action resource lock",
                    "Acquiring resources for: Genrule",
                    5,
                    5,
                ),
                CACHE_CHECK_LATE,
                ("Local execution process wall time", "Genrule", 30, 60),
            ],
        );
        assert_eq!(
            entered(&a),
            [(Phase::Preparing, 0), (Phase::Scheduling, 5), (LOCAL, 30)]
        );
        assert_eq!(a.state_at(at(25)), State::In(Phase::Scheduling));
    }

    #[test]
    fn a_remote_attempt_falling_back_to_a_local_run_runs_on_each_in_turn() {
        let a = attempt(
            true,
            &[
                ("remote action execution", "execute remotely", 10, 5),
                ("local action execution", "TestRunner", 20, 70),
            ],
        );
        assert_eq!(
            entered(&a),
            [(Phase::Preparing, 0), (LOCAL, 20), (REMOTE, 10)]
        );
        assert_eq!(a.state_at(at(12)), State::In(REMOTE));
        assert_eq!(a.state_at(at(50)), State::In(LOCAL));
    }

    #[test]
    fn a_hit_after_waiting_to_build_the_remote_action_is_done_cached() {
        let a = attempt(
            true,
            &[
                ACTION_EXECUTE,
                ("Remote execution setup", "acquiring semaphore", 2, 60),
                CACHE_CHECK_LATE,
                ("remote output download", "download outputs", 70, 5),
            ],
        );
        assert_eq!(
            entered(&a),
            [(Phase::Preparing, 0), (Phase::CheckingCache, 20)]
        );
        assert_eq!(a.state_at(at(30)), State::In(Phase::CheckingCache));
        assert_eq!(a.state_at(at(100)), State::Done(Done::Cached));
    }

    #[test]
    fn a_sandboxed_run_too_brief_for_its_own_span_runs_by_its_postcondition_check() {
        let a = attempt(
            false,
            &[
                ACTION_EXECUTE,
                CACHE_CHECK,
                ("general information", "context.prefetchInputs", 8, 1),
                ("general information", "sandbox.verifyPostCondition", 40, 1),
                ("general information", "sandbox.delete", 45, 1),
            ],
        );
        assert_eq!(
            entered(&a),
            [
                (Phase::Preparing, 0),
                (Phase::CheckingCache, 1),
                (LOCAL, 40)
            ]
        );
        assert_eq!(a.state_at(at(100)), State::Done(Done::Executed));
    }

    #[test]
    fn only_waiting_for_local_resources_among_resource_locks_schedules() {
        let a = attempt(
            false,
            &[
                (
                    "action resource lock",
                    "action.enterActionPreparation",
                    2,
                    10,
                ),
                CACHE_CHECK_LATE,
            ],
        );
        assert_eq!(
            entered(&a),
            [(Phase::Preparing, 0), (Phase::CheckingCache, 20)]
        );
        assert_eq!(a.state_at(at(100)), State::Done(Done::Cached));
    }

    #[test]
    fn an_action_executed_again_is_a_second_attempt() {
        let hit: &[Span] = &[CACHE_CHECK];
        let ran: &[Span] = &[("general information", "subprocess.run", 10, 50)];
        let attempts = attempts(false, &[hit, ran]);
        assert_eq!(
            attempts
                .iter()
                .map(|a| (a.start(), a.done()))
                .collect::<Vec<_>>(),
            [(at(0), Done::Cached), (at(1000), Done::Executed)]
        );
    }
}
