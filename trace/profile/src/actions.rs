use std::time::Duration;

use bviz_invocation::{ConfiguredTarget, Profiled, Runner, Spawns, Timings};

use crate::model::{CategoryId, LaneClass, OptStr, Profile, Slice};

/// One action span the profile records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Action {
    /// The action's primary output (see [`crate::SliceDetail::out`]).
    pub out: OptStr,
    pub target: OptStr,
    pub configuration: OptStr,
    pub wall: Duration,
    pub spawns: Spawns,
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

/// What the spans of one Bazel `cat` nested in an action's span say of its
/// runner.
#[derive(Debug, Clone, Copy)]
enum Evidence {
    Silent,
    Of(Runner),
    /// Left to the span's name: only a sandboxed subprocess says anything.
    Named,
}

impl Evidence {
    fn of(category: &str, cache_hit: Runner) -> Evidence {
        use Runner::*;
        match category {
            "remote action execution"
            | "Remote execution process wall time"
            | "Remote execution queuing time"
            | "Remote execution setup" => Evidence::Of(RemoteExecution),
            "local execution in worker"
            | "setting up inputs for worker"
            | "borrowing a worker"
            | "waiting for response from worker"
            | "copying outputs from worker" => Evidence::Of(Worker),
            "local action execution" | "Local execution process wall time" => Evidence::Of(Local),
            "Sandbox execution process wall time" => Evidence::Of(Sandboxed),
            "remote action cache check" => Evidence::Of(cache_hit),
            "general information" => Evidence::Named,
            _ => Evidence::Silent,
        }
    }

    fn runner(self, profile: &Profile, slice: &Slice) -> Option<Runner> {
        match self {
            Evidence::Silent => None,
            Evidence::Of(runner) => Some(runner),
            Evidence::Named => {
                (profile.str(slice.name) == SANDBOXED_SUBPROCESS_SPAN).then_some(Runner::Sandboxed)
            }
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
        let runner_of = |slice: &Slice| {
            slice
                .category
                .and_then(|id| evidence[id.position()].runner(self, slice))
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
                // By runner, when the last span counted for it ends: a span of
                // the runner starting before then is nested in that one, whose
                // time covers its own.
                let mut counted_until = [i64::MIN; Runner::ALL.len()];
                // A span's descendants are the run after it nested deeper.
                for nested in slices[i + 1..]
                    .iter()
                    .take_while(|nested| nested.depth > slice.depth)
                {
                    let Some(runner) = runner_of(nested) else {
                        continue;
                    };
                    let until = &mut counted_until[runner.index()];
                    if nested.start_us >= *until {
                        *until = nested.end_us();
                        spawns.ran(runner, micros(nested.dur_us));
                    }
                }
                let action = Action {
                    out: detail.out,
                    target: detail.target,
                    configuration: detail.configuration,
                    wall: micros(slice.dur_us),
                    spawns,
                };
                started.push((slice.start_us, action));
            }
        }
        started.sort_by_key(|&(start_us, _)| start_us);
        started.into_iter().map(|(_, action)| action).collect()
    }

    /// The time each of [its actions](Self::actions) took and where it ran.
    pub fn timings(&self) -> Timings {
        let actions = self.actions();
        let mut timings = Timings::with_capacity(actions.len());
        for action in &actions {
            let owner = self
                .opt_str(action.target)
                .map(|label| ConfiguredTarget::new(label, self.opt_str(action.configuration)));
            timings.record(Profiled {
                primary_output: self.opt_str(action.out),
                owner,
                wall: action.wall,
                spawns: action.spawns,
            });
        }
        timings
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
    use bviz_invocation::Timing;

    const ACTION_TIDS: [i64; 2] = [2, 3];
    const MAIN_TID: i64 = 1;

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
                r#"{{"cat":"{ACTION_CATEGORY}","name":"a","ph":"X","ts":{at},"dur":100,"tid":{tid}{out},"args":{{"target":"//p:t","configuration":"c"}}}}"#
            ));
            for (cat, name, ts, dur) in *nested {
                events.push(format!(
                    r#"{{"cat":"{cat}","name":"{name}","ph":"X","ts":{},"dur":{dur},"tid":{tid}}}"#,
                    at + ts
                ));
            }
            at += 1000;
        }
        let json = format!(r#"{{"traceEvents":[{}]}}"#, events.join(","));
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
            (p.opt_str(a.target), p.opt_str(a.configuration), a.wall),
            (Some("//p:t"), Some("c"), Duration::from_micros(100))
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
}
