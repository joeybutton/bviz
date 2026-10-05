use super::*;
use bviz_invocation::spawn::LoggedSpawns;

const CRITICAL_PATH_TID: i64 = 0;
const MAIN_TID: i64 = 1;
const EXECUTION_TIDS: [i64; 2] = [2, 3];

/// A profile spanning `wall` microseconds on its main thread, with the
/// critical-path lane holding `steps` and the execution threads taking turns
/// to hold `spans`.
fn profile(wall: i64, steps: &[Event], spans: &[Event]) -> Profile {
    let mut events = vec![
        thread(CRITICAL_PATH_TID, "Critical Path"),
        thread(MAIN_TID, "Main Thread"),
        format!(
            r#"{{"cat":"general information","name":"buildTargets","ph":"X","ts":0,"dur":{wall},"tid":{MAIN_TID}}}"#
        ),
    ];
    for (i, tid) in EXECUTION_TIDS.into_iter().enumerate() {
        events.push(thread(tid, &format!("skyframe-evaluator-execution-{i}")));
    }
    events.extend(
        steps
            .iter()
            .map(|step| step.json("critical path component", CRITICAL_PATH_TID)),
    );
    events.extend(
        spans
            .iter()
            .zip(EXECUTION_TIDS.into_iter().cycle())
            .map(|(span, tid)| span.json("action processing", tid)),
    );
    let json = format!(r#"{{"traceEvents":[{}]}}"#, events.join(","));
    Profile::from_bytes(json.as_bytes()).expect("parse")
}

fn thread(tid: i64, name: &str) -> String {
    format!(r#"{{"name":"thread_name","ph":"M","tid":{tid},"args":{{"name":"{name}"}}}}"#)
}

/// An event of the critical-path lane or an action span, with what Bazel may
/// write of the action it stands for.
#[derive(Clone, Copy)]
struct Event<'a> {
    name: &'a str,
    ts: i64,
    dur: i64,
    out: Option<&'a str>,
    mnemonic: Option<&'a str>,
    target: Option<&'a str>,
}

impl<'a> Event<'a> {
    fn json(self, cat: &str, tid: i64) -> String {
        let Event { name, ts, dur, .. } = self;
        // Bazel writes a step of no length as an instant.
        let timing = if dur == 0 {
            format!(r#""ph":"i","ts":{ts}"#)
        } else {
            format!(r#""ph":"X","ts":{ts},"dur":{dur}"#)
        };
        let out = self
            .out
            .map_or(String::new(), |out| format!(r#","out":"{out}""#));
        let args: Vec<String> = [("mnemonic", self.mnemonic), ("target", self.target)]
            .into_iter()
            .filter_map(|(key, value)| Some(format!(r#""{key}":"{}""#, value?)))
            .collect();
        format!(
            r#"{{"cat":"{cat}","name":"{name}",{timing},"tid":{tid}{out},"args":{{{}}}}}"#,
            args.join(",")
        )
    }

    fn with_out(self, out: &'a str) -> Self {
        Event {
            out: Some(out),
            ..self
        }
    }

    fn of(self, mnemonic: &'a str, target: &'a str) -> Self {
        Event {
            mnemonic: Some(mnemonic),
            target: Some(target),
            ..self
        }
    }
}

fn event(name: &str, ts: i64, dur: i64) -> Event<'_> {
    Event {
        name,
        ts,
        dur,
        out: None,
        mnemonic: None,
        target: None,
    }
}

const COMPILE: &str = "Compiling a.c";
const STEP_COMPILE: &str = "action 'Compiling a.c'";
const LINK: &str = "Linking a";
const STEP_LINK: &str = "action 'Linking a'";

fn us(micros: u64) -> Duration {
    Duration::from_micros(micros)
}

fn split(before: u64, span: u64) -> Work {
    Work::Spanned(SpanSplit {
        before: us(before),
        span: Ran::Unlogged(us(span)),
    })
}
/// The lead-in, each step's gap and own time, and the tail add up to the
/// wall time, as do the parts.
fn assert_partitions(path: &CriticalPath) {
    let along: Duration = path.lead_in()
        + path
            .steps()
            .iter()
            .map(|step| step.gap() + step.own())
            .sum::<Duration>()
        + path.tail();
    assert_eq!(along, path.wall());
    let by_part: Duration = Part::ALL.into_iter().map(|part| path.time_in(part)).sum();
    assert_eq!(by_part, path.wall());
    for step in path.steps() {
        let parts: Duration = step.parts().map(|(_, time)| time).sum();
        assert_eq!(parts, step.gap() + step.own());
    }
}

#[test]
fn a_profile_without_a_critical_path_lane_has_no_critical_path() {
    let json = format!(
        r#"{{"traceEvents":[{},{}]}}"#,
        thread(MAIN_TID, "Main Thread"),
        event("buildTargets", 0, 10).json("general information", MAIN_TID)
    );
    let profile = Profile::from_bytes(json.as_bytes()).expect("parse");
    assert_eq!(CriticalPath::of(&profile, None), None);
}

#[test]
fn a_step_is_joined_to_the_span_of_its_description_and_split_around_it() {
    // As Bazel 9 writes it: the step names the action only by description,
    // and its span ends a little after the step does.
    let p = profile(
        2000,
        &[event(STEP_COMPILE, 100, 1000)],
        &[event(COMPILE, 105, 1001)
            .with_out("bazel-out/a.o")
            .of("CppCompile", "//a")],
    );
    let path = CriticalPath::of(&p, None).expect("critical path");
    assert_partitions(&path);
    let [step] = path.steps() else {
        panic!("one step")
    };
    assert_eq!(step.name(), COMPILE);
    assert_eq!(step.mnemonic(), Some("CppCompile"));
    assert_eq!(step.target(), Some("//a"));
    assert_eq!(step.primary_output(), Some("bazel-out/a.o"));
    assert_eq!(step.work(), split(5, 995));
    assert_eq!((path.lead_in(), path.tail()), (us(100), us(900)));
    assert_eq!(path.time_in(Part::BeforeAction), us(5));
    assert_eq!(path.time_in(Part::Action), us(995));
}

#[test]
fn the_action_runs_until_its_step_ends_though_its_span_outlasts_it() {
    // As Bazel writes it: the step ends from within the span, so the span
    // starts a fraction of a millisecond after the step and ends about a
    // millisecond after it.
    let p = profile(
        500_000,
        &[event(STEP_LINK, 10_000, 416_000)],
        &[event(LINK, 10_250, 416_950)],
    );
    let path = CriticalPath::of(&p, None).expect("critical path");
    assert_partitions(&path);
    assert_eq!(path.steps()[0].work(), split(250, 415_750));
    assert_eq!(path.tail(), us(74_000));
}

#[test]
fn a_step_naming_its_primary_output_is_joined_by_it() {
    // As Bazel 10 writes it: the step carries the action's output, mnemonic
    // and target. Two spans of its description start within it; only the
    // second writes its output.
    let p = profile(
        2000,
        &[event(STEP_COMPILE, 100, 1000)
            .with_out("bazel-out/b.o")
            .of("CppCompile", "//b")],
        &[
            event(COMPILE, 102, 10).with_out("bazel-out/a.o"),
            event(COMPILE, 120, 990).with_out("bazel-out/b.o"),
        ],
    );
    let path = CriticalPath::of(&p, None).expect("critical path");
    assert_partitions(&path);
    let step = &path.steps()[0];
    assert_eq!(step.work(), split(20, 980));
    assert_eq!(step.primary_output(), Some("bazel-out/b.o"));
    assert_eq!(step.target(), Some("//b"));
}

#[test]
fn steps_of_one_description_are_told_apart_by_when_their_spans_started() {
    let p = profile(
        5000,
        &[
            event(STEP_COMPILE, 100, 1000),
            event(STEP_COMPILE, 2000, 1000),
        ],
        &[event(COMPILE, 2003, 998), event(COMPILE, 104, 997)],
    );
    let path = CriticalPath::of(&p, None).expect("critical path");
    assert_partitions(&path);
    let works: Vec<Work> = path.steps().iter().map(Step::work).collect();
    assert_eq!(works, [split(4, 996), split(3, 997)]);
    assert_eq!(path.steps()[1].gap(), us(900));
    assert_eq!(path.longest_wait().map(Step::start_us), Some(2000));
}

#[test]
fn a_span_starting_outside_its_step_is_not_its_span() {
    let p = profile(
        5000,
        &[event(STEP_COMPILE, 100, 1000)],
        &[event(COMPILE, 3000, 500)],
    );
    let path = CriticalPath::of(&p, None).expect("critical path");
    assert_eq!(
        path.steps()[0].work(),
        Work::Unspanned(Ran::Unlogged(us(1000)))
    );
    assert_eq!(path.time_in(Part::Unspanned), us(1000));
}

const RUNFILES_STEP: &str = "runfiles for //t:bin";
const RUNFILES: &str = "RunfilesTree t/bin.runfiles";
/// Another target's runfiles tree, built alongside.
const OTHER_RUNFILES: &str = "RunfilesTree t/bin_test.runfiles";

#[test]
fn a_runfiles_tree_is_joined_to_the_span_building_its_target_s_tree() {
    // Two trees' spans start at one instant within the step.
    let p = profile(
        2000,
        &[event(RUNFILES_STEP, 100, 300)],
        &[
            event(OTHER_RUNFILES, 169, 280).of("RunfilesTree", "//t:bin_test"),
            event(RUNFILES, 169, 332)
                .with_out("bazel-out/k8-fastbuild/bin/t/bin.runfiles")
                .of("RunfilesTree", "//t:bin"),
        ],
    );
    let path = CriticalPath::of(&p, None).expect("critical path");
    assert_partitions(&path);
    let step = &path.steps()[0];
    assert_eq!(step.work(), split(69, 231));
    assert_eq!(step.name(), RUNFILES_STEP);
    assert_eq!(step.target(), Some("//t:bin"));
    assert_eq!(step.mnemonic(), Some("RunfilesTree"));
    assert_eq!(
        step.primary_output(),
        Some("bazel-out/k8-fastbuild/bin/t/bin.runfiles")
    );
}

#[test]
fn a_runfiles_tree_is_joined_by_its_directory_where_spans_name_no_target() {
    let p = profile(
        2000,
        &[event(RUNFILES_STEP, 100, 300)],
        &[event(OTHER_RUNFILES, 169, 280), event(RUNFILES, 169, 332)],
    );
    let path = CriticalPath::of(&p, None).expect("critical path");
    let step = &path.steps()[0];
    assert_eq!(step.work(), split(69, 231));
    assert_eq!(step.target(), Some("//t:bin"));
}

#[test]
fn a_runfiles_tree_of_another_repository_joins_only_by_its_target() {
    let p = profile(
        2000,
        &[event("runfiles for @@r+//t:bin", 100, 300)],
        &[event(RUNFILES, 169, 332)],
    );
    let path = CriticalPath::of(&p, None).expect("critical path");
    assert_eq!(
        path.steps()[0].work(),
        Work::Unspanned(Ran::Unlogged(us(300)))
    );
    assert_eq!(path.steps()[0].target(), Some("@@r+//t:bin"));
}

#[test]
fn a_runfiles_tree_of_no_length_was_change_pruned() {
    let p = profile(
        2000,
        &[event(RUNFILES_STEP, 100, 0)],
        &[event(RUNFILES, 100, 50).of("RunfilesTree", "//t:bin")],
    );
    let path = CriticalPath::of(&p, None).expect("critical path");
    assert_eq!(path.steps()[0].work(), Work::ChangePruned);
}

#[test]
fn of_two_spans_of_one_description_starting_within_a_step_the_earlier_is_its() {
    // Nothing tells the two apart without a primary output: the earlier is
    // taken, though either could have run the step.
    let p = profile(
        5000,
        &[
            event(STEP_COMPILE, 100, 1000),
            event(STEP_COMPILE, 2000, 1000),
        ],
        &[event(COMPILE, 102, 10), event(COMPILE, 120, 990)],
    );
    let path = CriticalPath::of(&p, None).expect("critical path");
    assert_partitions(&path);
    let works: Vec<Work> = path.steps().iter().map(Step::work).collect();
    assert_eq!(
        works,
        [split(2, 998), Work::Unspanned(Ran::Unlogged(us(1000)))]
    );
}

#[test]
fn a_step_of_no_length_was_change_pruned_and_claims_no_span() {
    let p = profile(
        3000,
        &[event(STEP_LINK, 500, 0), event(STEP_LINK, 1000, 600)],
        &[event(LINK, 1005, 590)],
    );
    let path = CriticalPath::of(&p, None).expect("critical path");
    assert_partitions(&path);
    let works: Vec<Work> = path.steps().iter().map(Step::work).collect();
    assert_eq!(works, [Work::ChangePruned, split(5, 595)]);
    assert_eq!(path.steps()[1].gap(), us(500));
}

#[test]
fn a_step_starting_before_the_one_before_it_ends_adds_only_what_runs_past() {
    let p = profile(
        3000,
        &[
            event(STEP_COMPILE, 100, 1000),
            event(STEP_LINK, 400, 0),
            event(STEP_LINK, 900, 500),
        ],
        &[event(COMPILE, 105, 996), event(LINK, 905, 496)],
    );
    let path = CriticalPath::of(&p, None).expect("critical path");
    assert_partitions(&path);
    let steps = path.steps();
    assert_eq!(steps[1].work(), Work::ChangePruned);
    assert_eq!((steps[2].gap(), steps[2].own()), (Duration::ZERO, us(300)));
    // The span starts in the time the step before already covers.
    assert_eq!(steps[2].work(), split(0, 300));
    assert_eq!(path.path(), us(1300));
}

#[test]
fn a_step_the_one_before_it_covers_still_claims_its_span() {
    let p = profile(
        3000,
        &[
            event(STEP_COMPILE, 100, 1000),
            event(STEP_LINK, 200, 700),
            event(STEP_LINK, 205, 1795),
        ],
        &[
            event(COMPILE, 105, 996),
            event(LINK, 210, 691),
            event(LINK, 1950, 51),
        ],
    );
    let path = CriticalPath::of(&p, None).expect("critical path");
    assert_partitions(&path);
    let steps = path.steps();
    assert_eq!(steps[1].own(), Duration::ZERO);
    assert_eq!(steps[1].work(), split(0, 0));
    assert_eq!(steps[2].work(), split(850, 50));
}

#[test]
fn the_wall_time_partitions_along_the_path() {
    let p = profile(
        10_000,
        &[
            event(STEP_COMPILE, 1000, 2000),
            event("runfiles for //a:a", 3500, 100),
            event(STEP_LINK, 4000, 0),
            event(STEP_LINK, 5000, 3000),
        ],
        &[event(COMPILE, 1010, 1991), event(LINK, 5050, 2951)],
    );
    let path = CriticalPath::of(&p, None).expect("critical path");
    assert_partitions(&path);
    assert_eq!(path.wall(), us(10_000));
    assert_eq!(path.lead_in(), us(1000));
    assert_eq!(path.time_in(Part::Waiting), us(500 + 400 + 1000));
    assert_eq!(path.time_in(Part::BeforeAction), us(10 + 50));
    assert_eq!(path.time_in(Part::Action), us(1990 + 2950));
    assert_eq!(path.time_in(Part::Unspanned), us(100));
    assert_eq!(path.tail(), us(2000));
    assert_eq!(path.path(), us(5100));
}

/// A spawn an execution log records, for [`log`].
#[derive(Clone, Copy)]
struct LogSpawn<'a> {
    runner: &'a str,
    cache_hit: bool,
    mnemonic: &'a str,
    target: &'a str,
    outputs: &'a [&'a str],
    total_us: u64,
    phases: &'a [(Phase, u64)],
}

fn logged<'a>(
    runner: &'a str,
    outputs: &'a [&'a str],
    total_us: u64,
    phases: &'a [(Phase, u64)],
) -> LogSpawn<'a> {
    LogSpawn {
        runner,
        cache_hit: false,
        mnemonic: "CppCompile",
        target: "//a",
        outputs,
        total_us,
        phases,
    }
}

fn log(spawns: &[LogSpawn]) -> LoggedSpawns {
    use bviz_invocation::spawn::{Builder, Logged, PhaseTiming, REPORTED_PHASES};
    let mut builder = Builder::with_capacity(spawns.len(), 0, 0);
    for spawn in spawns {
        for output in spawn.outputs {
            builder.output(output);
        }
        let mut reported = [Duration::ZERO; REPORTED_PHASES];
        for &(phase, time) in spawn.phases {
            reported[phase.index()] = us(time);
        }
        builder.push(Logged {
            runner: spawn.runner,
            mnemonic: spawn.mnemonic,
            target_label: spawn.target,
            cache_hit: spawn.cache_hit,
            timing: PhaseTiming::new(us(spawn.total_us), reported),
        });
    }
    builder.finish()
}

/// The spawns joined to `step`.
fn spawned(step: &Step) -> Spawned {
    *step.work().spawned().expect("spawns joined to the step")
}

/// A step's action time partitions into its spawns' phases and the time
/// around them.
fn assert_action_partitions(step: &Step) {
    let spawned = spawned(step);
    let phases: Duration = Phase::ALL.into_iter().map(|p| spawned.phase(p)).sum();
    let action = match step.work() {
        Work::Spanned(split) => split.span.time(),
        Work::Unspanned(own) => own.time(),
        Work::ChangePruned => Duration::ZERO,
    };
    assert_eq!(phases + spawned.around(), action);
}

#[test]
fn a_step_naming_its_output_is_joined_to_the_spawn_listing_it_among_others() {
    let p = profile(
        5000,
        &[event(STEP_COMPILE, 100, 1000)
            .with_out("bazel-out/a.o")
            .of("CppCompile", "//a")],
        &[event(COMPILE, 110, 991).with_out("bazel-out/a.o")],
    );
    let spawns = log(&[
        logged("remote", &["bazel-out/b.o"], 500, &[(Phase::Execute, 500)]),
        logged(
            "remote",
            &["bazel-out/a.d", "bazel-out/a.o"],
            960,
            &[
                (Phase::Upload, 100),
                (Phase::Queue, 60),
                (Phase::Execute, 700),
            ],
        ),
    ]);
    let path = CriticalPath::of(&p, Some(&spawns)).expect("critical path");
    assert_partitions(&path);
    let step = &path.steps()[0];
    assert_action_partitions(step);
    let spawned = spawned(step);
    assert_eq!(spawned.runner(), SpawnRunner::RemoteExecution);
    assert_eq!((spawned.spawns(), spawned.logged()), (1, us(960)));
    assert_eq!(spawned.phase(Phase::Execute), us(700));
    assert_eq!(spawned.phase(Phase::Other), us(100));
    assert_eq!(spawned.fit(), Fit::Within { around: us(30) });
    let coverage = path.coverage().expect("a log was given");
    assert_eq!((coverage.steps(), coverage.own()), (1, us(1000)));
    assert_eq!(coverage.action(), us(990));
    assert_eq!(coverage.phase(Phase::Queue), us(60));
}

#[test]
fn a_step_named_by_description_is_joined_by_its_span_s_output() {
    let p = profile(
        2000,
        &[event(STEP_COMPILE, 100, 1000)],
        &[event(COMPILE, 105, 1001).with_out("bazel-out/a.o")],
    );
    let spawns = log(&[logged(
        "linux-sandbox",
        &["bazel-out/a.o"],
        900,
        &[(Phase::Execute, 900)],
    )]);
    let path = CriticalPath::of(&p, Some(&spawns)).expect("critical path");
    assert_partitions(&path);
    let spawned = spawned(&path.steps()[0]);
    assert_eq!(spawned.runner(), SpawnRunner::Sandboxed);
    assert_eq!(spawned.fit(), Fit::Within { around: us(95) });
}

#[test]
fn a_test_is_joined_to_the_spawns_writing_beside_its_log() {
    const TEST_LOG: &str = "bazel-out/testlogs/t/t_test/test.log";
    let p = profile(
        9000,
        &[event("action 'Testing //t:t_test'", 100, 3000)],
        &[event("Testing //t:t_test", 102, 2999)
            .with_out(TEST_LOG)
            .of("TestRunner", "//t:t_test")],
    );
    let test = |outputs, total| LogSpawn {
        mnemonic: "TestRunner",
        target: "//t:t_test",
        ..logged("remote", outputs, total, &[])
    };
    let spawns = log(&[
        test(
            &[
                "bazel-out/testlogs/t/t_test/test.xml",
                "bazel-out/testlogs/t/t_test/test.warnings",
            ],
            2000,
        ),
        test(&["bazel-out/testlogs/t/t_test/test.xml"], 500),
        test(&["bazel-out/testlogs/t/t_test/shard_2_of_2/test.xml"], 700),
        LogSpawn {
            target: "//t:other_test",
            ..test(&["bazel-out/testlogs/t/t_test/test.xml"], 400)
        },
    ]);
    let path = CriticalPath::of(&p, Some(&spawns)).expect("critical path");
    assert_partitions(&path);
    let step = &path.steps()[0];
    assert_action_partitions(step);
    let spawned = spawned(step);
    assert_eq!((spawned.spawns(), spawned.logged()), (2, us(2500)));
    assert_eq!(spawned.phase(Phase::Other), us(2500));
}

#[test]
fn a_test_whose_log_is_unknown_is_joined_to_no_spawn() {
    let p = profile(
        9000,
        &[event("action 'Testing //t:t_test'", 100, 3000).of("TestRunner", "//t:t_test")],
        &[],
    );
    let spawns = log(&[LogSpawn {
        mnemonic: "TestRunner",
        target: "//t:t_test",
        ..logged(
            "remote",
            &["bazel-out/testlogs/t/t_test/test.xml"],
            2000,
            &[],
        )
    }]);
    let path = CriticalPath::of(&p, Some(&spawns)).expect("critical path");
    assert_eq!(path.steps()[0].work().spawned(), None);
}

#[test]
fn an_action_ran_on_what_executed_it_longest_and_failing_that_what_answered_it() {
    let p = profile(
        5000,
        &[event(STEP_COMPILE, 100, 1000).with_out("bazel-out/a.o")],
        &[],
    );
    let runner = |spawns: &[LogSpawn]| {
        let path = CriticalPath::of(&p, Some(&log(spawns))).expect("critical path");
        spawned(&path.steps()[0]).runner()
    };
    let deduplicated = logged("deduplicated", &["bazel-out/a.o"], 900, &[]);
    let hit = LogSpawn {
        cache_hit: true,
        ..logged("disk cache hit", &["bazel-out/a.o"], 50, &[])
    };
    let remote = logged("remote", &["bazel-out/a.o"], 20, &[]);
    let sandboxed = logged("linux-sandbox", &["bazel-out/a.o"], 300, &[]);
    assert_eq!(runner(&[deduplicated]), SpawnRunner::Deduplicated);
    assert_eq!(runner(&[hit, deduplicated]), SpawnRunner::Deduplicated);
    assert_eq!(
        runner(&[deduplicated, remote]),
        SpawnRunner::RemoteExecution
    );
    assert_eq!(runner(&[remote, sandboxed]), SpawnRunner::Sandboxed);
}

#[test]
fn a_spawn_cache_hit_shows_its_transfer() {
    let p = profile(
        2000,
        &[event(STEP_COMPILE, 100, 500).with_out("bazel-out/a.o")],
        &[],
    );
    let spawns = log(&[LogSpawn {
        cache_hit: true,
        ..logged(
            "remote cache hit",
            &["bazel-out/a.o"],
            480,
            &[(Phase::Network, 400), (Phase::Fetch, 20)],
        )
    }]);
    let path = CriticalPath::of(&p, Some(&spawns)).expect("critical path");
    assert_partitions(&path);
    let step = &path.steps()[0];
    assert!(matches!(step.work(), Work::Unspanned(Ran::Spawned(_))));
    assert_action_partitions(step);
    let spawned = spawned(step);
    assert_eq!(spawned.runner(), SpawnRunner::RemoteCacheHit);
    assert_eq!(spawned.phase(Phase::Network), us(400));
    assert_eq!(spawned.fit(), Fit::Within { around: us(20) });
}

#[test]
fn spawns_adding_up_past_the_span_are_scaled_to_fill_it() {
    let p = profile(
        2000,
        &[event(STEP_COMPILE, 100, 1000).with_out("bazel-out/a.o")],
        &[event(COMPILE, 100, 1001).with_out("bazel-out/a.o")],
    );
    let spawns = log(&[
        logged("remote", &["bazel-out/a.o"], 900, &[(Phase::Execute, 900)]),
        logged(
            "linux-sandbox",
            &["bazel-out/a.o"],
            601,
            &[(Phase::Execute, 600)],
        ),
    ]);
    let path = CriticalPath::of(&p, Some(&spawns)).expect("critical path");
    assert_partitions(&path);
    let step = &path.steps()[0];
    assert_action_partitions(step);
    let spawned = spawned(step);
    assert_eq!(spawned.fit(), Fit::Scaled);
    assert_eq!(spawned.runner(), SpawnRunner::RemoteExecution);
    assert_eq!(spawned.logged(), us(1501));
    // 1501µs of phases scaled into the 1000µs span, the nanosecond of
    // rounding going to the longest phase.
    assert_eq!(
        spawned.phase(Phase::Execute),
        us(1000) - spawned.phase(Phase::Other)
    );
    assert_eq!(spawned.phase(Phase::Other), Duration::from_nanos(666));
}

#[test]
fn without_a_log_no_step_is_joined_and_with_one_every_unjoined_step_says_so() {
    let p = profile(
        2000,
        &[event(STEP_COMPILE, 100, 1000).with_out("bazel-out/a.o")],
        &[event(COMPILE, 110, 991).with_out("bazel-out/a.o")],
    );
    let alone = CriticalPath::of(&p, None).expect("critical path");
    assert_eq!(alone.coverage(), None);
    assert_eq!(alone.steps()[0].work(), split(10, 990));

    let unrelated = log(&[logged("remote", &["bazel-out/b.o"], 5, &[])]);
    let path = CriticalPath::of(&p, Some(&unrelated)).expect("critical path");
    let coverage = path.coverage().expect("a log was given");
    assert_eq!((coverage.steps(), coverage.own()), (0, Duration::ZERO));
    assert_eq!(path.steps()[0].work(), split(10, 990));
    assert_eq!(
        path,
        CriticalPath {
            coverage: Some(coverage),
            ..alone
        }
    );
}

#[test]
fn a_change_pruned_step_is_joined_to_no_spawn() {
    let p = profile(
        2000,
        &[event(STEP_COMPILE, 100, 0).with_out("bazel-out/a.o")],
        &[],
    );
    let spawns = log(&[logged("remote", &["bazel-out/a.o"], 5, &[])]);
    let path = CriticalPath::of(&p, Some(&spawns)).expect("critical path");
    assert_eq!(path.steps()[0].work(), Work::ChangePruned);
    assert_eq!(path.coverage().map(|c| c.steps()), Some(0));
}

/// A remote compile, then a link run in a local sandbox after a wait, then
/// a step with neither span nor spawn; the profile starts 1000µs before the
/// first step.
fn levered_profile() -> Profile {
    profile(
        10_000,
        &[
            event(STEP_COMPILE, 1000, 3000).with_out("bazel-out/a.o"),
            event(STEP_LINK, 5000, 2000).with_out("bazel-out/a"),
            event("action 'Compiling b.c'", 7000, 500),
        ],
        &[
            event(COMPILE, 1000, 3001).with_out("bazel-out/a.o"),
            event(LINK, 5000, 2001).with_out("bazel-out/a"),
        ],
    )
}

/// The local link of [`levered_profile`].
fn local_link<'a>() -> LogSpawn<'a> {
    logged(
        "linux-sandbox",
        &["bazel-out/a"],
        2000,
        &[
            (Phase::Setup, 100),
            (Phase::Queue, 50),
            (Phase::Execute, 1850),
        ],
    )
}

#[test]
fn levers_rank_what_would_shorten_the_path() {
    let p = levered_profile();
    let spawns = log(&[
        logged(
            "remote",
            &["bazel-out/a.o"],
            3000,
            &[
                (Phase::Queue, 400),
                (Phase::Upload, 300),
                (Phase::Fetch, 200),
                (Phase::Execute, 2100),
            ],
        ),
        local_link(),
    ]);
    let path = CriticalPath::of(&p, Some(&spawns)).expect("critical path");
    assert_eq!(
        path.levers(),
        [
            (Lever::Execution, us(3950)),
            (Lever::LocalOnRemote, us(2000)),
            (Lever::LeadIn, us(1000)),
            (Lever::Waiting, us(1000)),
            (Lever::CacheHitOverhead, us(500)),
            (Lever::RemoteQueue, us(400)),
            (Lever::Upload, us(300)),
            (Lever::Fetch, us(200)),
            (Lever::LocalSetup, us(100)),
            (Lever::LocalQueue, us(50)),
        ]
    );
}

#[test]
fn without_a_log_only_the_profile_s_levers_act() {
    let alone = CriticalPath::of(&levered_profile(), None).expect("critical path");
    let levers: Vec<Lever> = alone.levers().into_iter().map(|(lever, _)| lever).collect();
    assert_eq!(levers, [Lever::LeadIn, Lever::Waiting]);
}

#[test]
fn a_build_answered_remotely_only_from_the_cache_has_no_local_steps_to_move() {
    let p = levered_profile();
    let spawns = log(&[
        LogSpawn {
            cache_hit: true,
            ..logged("remote cache hit", &["bazel-out/a.o"], 3000, &[])
        },
        local_link(),
    ]);
    let path = CriticalPath::of(&p, Some(&spawns)).expect("critical path");
    assert!(
        path.levers()
            .iter()
            .all(|&(lever, _)| lever != Lever::LocalOnRemote)
    );
}

#[test]
fn a_remote_execution_off_the_path_shows_local_steps_could_move() {
    let p = levered_profile();
    let spawns = log(&[
        logged("remote", &["bazel-out/elsewhere.o"], 10, &[]),
        local_link(),
    ]);
    let path = CriticalPath::of(&p, Some(&spawns)).expect("critical path");
    assert!(path.levers().contains(&(Lever::LocalOnRemote, us(2000))));
}
