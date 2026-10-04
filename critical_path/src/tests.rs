use super::*;

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
        span: us(span),
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
    assert_eq!(CriticalPath::of(&profile), None);
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
    let path = CriticalPath::of(&p).expect("critical path");
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
    let path = CriticalPath::of(&p).expect("critical path");
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
    let path = CriticalPath::of(&p).expect("critical path");
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
    let path = CriticalPath::of(&p).expect("critical path");
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
    let path = CriticalPath::of(&p).expect("critical path");
    assert_eq!(path.steps()[0].work(), Work::Unspanned(us(1000)));
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
    let path = CriticalPath::of(&p).expect("critical path");
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
    let path = CriticalPath::of(&p).expect("critical path");
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
    let path = CriticalPath::of(&p).expect("critical path");
    assert_eq!(path.steps()[0].work(), Work::Unspanned(us(300)));
    assert_eq!(path.steps()[0].target(), Some("@@r+//t:bin"));
}

#[test]
fn a_runfiles_tree_of_no_length_was_change_pruned() {
    let p = profile(
        2000,
        &[event(RUNFILES_STEP, 100, 0)],
        &[event(RUNFILES, 100, 50).of("RunfilesTree", "//t:bin")],
    );
    let path = CriticalPath::of(&p).expect("critical path");
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
    let path = CriticalPath::of(&p).expect("critical path");
    assert_partitions(&path);
    let works: Vec<Work> = path.steps().iter().map(Step::work).collect();
    assert_eq!(works, [split(2, 998), Work::Unspanned(us(1000))]);
}

#[test]
fn a_step_of_no_length_was_change_pruned_and_claims_no_span() {
    let p = profile(
        3000,
        &[event(STEP_LINK, 500, 0), event(STEP_LINK, 1000, 600)],
        &[event(LINK, 1005, 590)],
    );
    let path = CriticalPath::of(&p).expect("critical path");
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
    let path = CriticalPath::of(&p).expect("critical path");
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
    let path = CriticalPath::of(&p).expect("critical path");
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
    let path = CriticalPath::of(&p).expect("critical path");
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
