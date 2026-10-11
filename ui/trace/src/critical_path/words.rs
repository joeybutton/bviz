//! How the parts of a critical path read on screen.

use crate::chart::format_duration;
use bviz_critical_path::{
    Coverage, CriticalPath, Fit, Lever, Modeled, NearlyCritical, Part, Phase, Ran, SpanSplit,
    SpawnRunner, Spawned, Step, StepModel, Work,
};
pub(crate) use bviz_ui_spawn_phase::phase_name;
use std::time::Duration;

/// What a share of the wall time is a share of, in a part's tooltip.
const OF_THE_WALL_TIME: &str = "the wall time";
/// What a share of the spawn phases' time is a share of, in a phase's tooltip.
pub(crate) const OF_THE_ACTIONS: &str = "these actions' time";
/// The name of an action's time outside its spawns.
pub(crate) const AROUND_SPAWNS: &str = "around spawns";
/// What a step's Waited cell shows where it waited no time.
const NO_WAIT: &str = "—";
/// The share of the profile's action spans under which so few match the
/// action graph that the modeled build's tooltip warns of it.
const FEW_MATCHED: f64 = 0.5;

pub(crate) fn part_name(part: Part) -> &'static str {
    match part {
        Part::LeadIn => "lead-in",
        Part::Waiting => "waiting",
        Part::BeforeAction => "before action",
        Part::Action => "action",
        Part::Unspanned => "no action span",
        Part::Tail => "tail",
    }
}

/// A word on what ran a step, where its action span leaves something to
/// say. `logged` says whether an execution log of the invocation was given.
pub(crate) fn work_tag(work: Work, logged: bool) -> Option<&'static str> {
    match work {
        Work::Spanned(SpanSplit {
            span: Ran::Spawned(spawned),
            ..
        })
        | Work::Unspanned(Ran::Spawned(spawned)) => Some(runner_name(spawned.runner())),
        Work::Spanned(_) => logged.then_some("no spawn"),
        Work::Unspanned(_) => Some("no span"),
        Work::ChangePruned => Some("pruned"),
    }
}

fn runner_name(runner: SpawnRunner) -> &'static str {
    match runner {
        SpawnRunner::RemoteExecution => "remote",
        SpawnRunner::Worker => "worker",
        SpawnRunner::Sandboxed => "sandbox",
        SpawnRunner::Local => "local",
        SpawnRunner::RemoteCacheHit => "remote cache",
        SpawnRunner::DiskCacheHit => "disk cache",
        SpawnRunner::Deduplicated => "deduplicated",
    }
}

/// How an action `runner` answered came by its result.
fn runner_phrase(runner: SpawnRunner) -> &'static str {
    match runner {
        SpawnRunner::RemoteExecution => "Ran remotely",
        SpawnRunner::Worker => "Ran on a persistent worker",
        SpawnRunner::Sandboxed => "Ran in a local sandbox",
        SpawnRunner::Local => "Ran locally",
        SpawnRunner::RemoteCacheHit => "Remote cache hit",
        SpawnRunner::DiskCacheHit => "Disk cache hit",
        SpawnRunner::Deduplicated => "Reused an identical spawn's outputs",
    }
}

/// What [`work_tag`] stands for.
fn work_note(work: Work, logged: bool) -> Option<String> {
    let note = match (work, logged) {
        (
            Work::Spanned(SpanSplit {
                span: Ran::Spawned(spawned),
                ..
            })
            | Work::Unspanned(Ran::Spawned(spawned)),
            _,
        ) => return Some(spawned_note(&spawned)),
        (Work::Spanned(_), false) => return None,
        (Work::Spanned(_), true) => {
            "No logged spawn: it ran none (as a file write) or went unlogged."
        }
        (Work::Unspanned(_), false) => "No action span: an action-cache hit, or not recorded.",
        (Work::Unspanned(_), true) => {
            "No action span or logged spawn: an action-cache hit, or not recorded."
        }
        (Work::ChangePruned, _) => "Change pruned: inputs unchanged, so it did not run.",
    };
    Some(note.to_owned())
}

/// What the execution log says of a step's spawns.
fn spawned_note(spawned: &Spawned) -> String {
    let spawns = spawned.spawns();
    let mut note = format!(
        "{}: {spawns} {}, {}.",
        runner_phrase(spawned.runner()),
        if spawns == 1 { "spawn" } else { "spawns" },
        duration(spawned.logged()),
    );
    if spawned.fit() == Fit::Scaled {
        note.push_str("\nPhases scaled down to the action's time.");
    }
    note
}

pub(crate) fn duration(d: Duration) -> String {
    format_duration(i64::try_from(d.as_micros()).unwrap_or(i64::MAX))
}

/// What the Waited cell of the step at `index` shows: nothing for the first,
/// whose wait is the lead-in.
pub(crate) fn waited(index: usize, gap: Duration) -> String {
    match index {
        0 => String::new(),
        _ if gap.is_zero() => NO_WAIT.to_owned(),
        _ => duration(gap),
    }
}

/// `part` as a percentage of `whole`.
pub(crate) fn share(part: Duration, whole: Duration) -> String {
    format!("{:.1}%", 100.0 * fraction(part, whole))
}

/// `part` of `whole`, and of nothing none.
pub(crate) fn fraction(part: Duration, whole: Duration) -> f64 {
    if whole.is_zero() {
        0.0
    } else {
        part.as_secs_f64() / whole.as_secs_f64()
    }
}

/// The path in sum: the wall time, the steps' own time and their count, and
/// how much of it the execution log accounts for where one is given.
pub(crate) fn headline(path: &CriticalPath) -> String {
    let steps = path.steps().len();
    let logged = path.coverage().map_or(String::new(), |coverage| {
        format!(
            " · spawns logged for {} of it",
            share(coverage.own(), path.path())
        )
    });
    format!(
        "Wall time {} · critical path {} ({} of it) over {steps} {}{logged}",
        duration(path.wall()),
        duration(path.path()),
        share(path.path(), path.wall()),
        if steps == 1 { "step" } else { "steps" },
    )
}

/// `part` with its time and share `of` what `whole` is, for its tooltip.
pub(crate) fn part_tip(part: Part, time: Duration, whole: Duration, of: &str) -> String {
    named_tip(part_name(part), time, whole, of)
}

/// What is `named`, with its time and share `of` what `whole` is.
pub(crate) fn named_tip(named: &str, time: Duration, whole: Duration, of: &str) -> String {
    format!(
        "{named}: {} ({} of {of})",
        duration(time),
        share(time, whole)
    )
}

/// What the bar of the spawn phases shows: the logged actions' time on the
/// path, a scale apart from the wall time's.
pub(crate) fn spawned_title(coverage: &Coverage) -> String {
    let steps = coverage.steps();
    format!(
        "Spawn phases — {steps} logged {}, {}",
        if steps == 1 { "step" } else { "steps" },
        duration(coverage.action())
    )
}

/// The phases `names` lists, taken together.
pub(crate) fn folded(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [name] => (*name).to_owned(),
        [first @ .., last] => format!("{} and {last}", first.join(", ")),
    }
}

/// What opens the line of levers.
pub(crate) const LEVERS: &str = "Levers:";

/// The most pulling a lever acting on `time` can shorten the modeled build
/// by, its `ceiling`, where that falls short of the time.
pub(crate) fn lever_bound(ceiling: Option<Duration>, time: Duration) -> Option<Duration> {
    ceiling.filter(|&ceiling| ceiling < time)
}

/// A lever with the time on the path it acts on, the [bound](lever_bound)
/// on what pulling it saves where there is one, and what pulls it.
pub(crate) fn lever(lever: Lever, time: Duration, bound: Option<Duration>) -> String {
    let (name, pull) = match lever {
        Lever::LeadIn => ("lead-in", "warm server, fewer repository fetches"),
        Lever::Waiting => ("waiting", "inputs off the path, more --jobs"),
        Lever::RemoteQueue => ("remote queue", "more executor capacity"),
        Lever::LocalQueue => ("local queue", "--local_resources"),
        Lever::Parse => ("request building", "fewer inputs per action"),
        Lever::Upload => ("uploads", "fewer locally built inputs"),
        Lever::Network => ("remote-cache calls", "a closer cache, fewer inputs"),
        Lever::RemoteSetup => ("remote setup", "fewer, smaller inputs"),
        Lever::LocalSetup => ("local setup", "workers, a cheaper sandbox"),
        Lever::Execution => ("execution", "a faster tool, or split the target"),
        Lever::ProcessOutputs => ("output processing", "fewer, smaller outputs"),
        Lever::Fetch => ("downloads", "--remote_download_minimal"),
        Lever::LocalOnRemote => ("local on a remote build", "check strategies and tags"),
        Lever::CacheHitOverhead => ("likely cache hits", "cheaper output checks"),
    };
    let bound = bound.map_or(String::new(), |bound| {
        format!(", saves at most {}", duration(bound))
    });
    format!("{name} {}{bound} → {pull}", duration(time))
}

/// A lever's tooltip: what its time is and how pulling it shortens the path,
/// and the [bound](lever_bound) on what that saves where there is one.
pub(crate) fn lever_account(lever: Lever, bound: Option<Duration>) -> String {
    let tip = lever_tip(lever);
    match bound {
        Some(bound) => format!(
            "{tip}\nSaves at most {}: other chains then bound the build.",
            duration(bound)
        ),
        None => tip.to_owned(),
    }
}

/// What the time a lever acts on is, and how pulling it shortens the path.
pub(crate) fn lever_tip(lever: Lever) -> &'static str {
    match lever {
        Lever::LeadIn => {
            "Startup, loading, analysis and repository fetches before the first step.\n\
             A warm server and prefetched or vendored repositories shorten it."
        }
        Lever::Waiting => {
            "Steps waiting for inputs built off the path, or for a free job slot.\n\
             Cut those dependencies, or raise --jobs."
        }
        Lever::RemoteQueue => {
            "Spawns queued for a remote executor.\n\
             More executor capacity shortens it."
        }
        Lever::LocalQueue => {
            "Spawns waiting for local CPU and memory.\n\
             Raise --local_resources, or have actions declare less."
        }
        Lever::Parse => {
            "Building remote execution requests, e.g. input Merkle trees.\n\
             Fewer inputs per action build faster."
        }
        Lever::Upload => {
            "Uploading inputs the remote cache lacked, excluding network time.\n\
             Build those inputs remotely too, or cache them."
        }
        Lever::Network => {
            "Waiting on remote-cache calls: lookups, missing-input checks, transfers.\n\
             A closer cache and fewer, smaller inputs shorten it."
        }
        Lever::RemoteSetup => {
            "Remote executors staging inputs.\n\
             Fewer, smaller inputs, or inputs cached on the executor, shorten it."
        }
        Lever::LocalSetup => {
            "Fetching inputs and setting up sandboxes or workers.\n\
             Persistent workers, a cheaper sandbox or fewer inputs shorten it."
        }
        Lever::Execution => {
            "Running the spawns' commands.\n\
             Use a faster tool, or split the target so its parts run in parallel."
        }
        Lever::ProcessOutputs => {
            "Moving outputs out of the sandbox, worker or remote executor.\n\
             Fewer, smaller outputs move faster."
        }
        Lever::Fetch => {
            "Downloading outputs, excluding network time.\n\
             --remote_download_minimal downloads only the outputs needed locally."
        }
        Lever::LocalOnRemote => {
            "Steps run locally in a build that ran other spawns remotely.\n\
             A strategy or a no-remote tag may keep them local."
        }
        Lever::CacheHitOverhead => {
            "Steps with no action span or logged spawn, likely action-cache hits, that \
             still took time.\n\
             Bazel spent it checking their inputs and outputs."
        }
    }
}

/// What an action's time in `phase` went to.
fn phase_explanation(phase: Phase) -> &'static str {
    match phase {
        Phase::Parse => "Building remote execution requests, e.g. input Merkle trees.",
        Phase::Upload => "Uploading inputs the remote cache lacked, excluding network time.",
        Phase::Network => "Remote-cache calls: lookups, missing-input checks, transfers.",
        Phase::Queue => "Waiting for a remote executor, or for local resources.",
        Phase::Setup => "Staging inputs, and setting up a local sandbox or worker.",
        Phase::Execute => "Running the spawns' commands.",
        Phase::ProcessOutputs => "Moving outputs out of where they ran.",
        Phase::Fetch => "Downloading outputs, excluding network time.",
        Phase::Retry => "Earlier attempts of retried spawns.",
        Phase::Other => "Spawn time in no phase, e.g. remote waits the executor leaves out.",
    }
}

/// The tooltip of `phase` in the spawn phases' bar and legend.
pub(crate) fn phase_account(phase: Phase, coverage: &Coverage) -> String {
    format!(
        "{}\n{}",
        named_tip(
            phase_name(phase),
            coverage.phase(phase),
            coverage.action(),
            OF_THE_ACTIONS
        ),
        phase_explanation(phase)
    )
}

/// The tooltip of the time around spawns in the spawn phases' bar and legend.
pub(crate) fn around_account(coverage: &Coverage) -> String {
    format!(
        "{}\nAction time outside spawns: cache lookups and stores, output cleanup and \
         checks, the execution log.",
        named_tip(
            AROUND_SPAWNS,
            coverage.around(),
            coverage.action(),
            OF_THE_ACTIONS
        )
    )
}

/// What the wall time in `part` went to.
fn explanation(part: Part, path: &CriticalPath) -> String {
    match part {
        Part::LeadIn => {
            "Startup, loading, analysis and repository fetches before the first step.".to_owned()
        }
        Part::Waiting => {
            let longest = path.longest_wait().map_or(String::new(), |step| {
                format!(
                    "\nLongest: {} before {}.",
                    duration(step.gap()),
                    step.name()
                )
            });
            format!(
                "Steps waiting for inputs off the path, input checks, or a free --jobs \
                 slot.{longest}"
            )
        }
        Part::BeforeAction => {
            "Before action spans started: input discovery and action-cache checks.".to_owned()
        }
        Part::Action => "The actions themselves.".to_owned(),
        Part::Unspanned => {
            "Steps with no action span: action-cache hits, or spans left out.".to_owned()
        }
        Part::Tail => "After the last step, until the command ended.".to_owned(),
    }
}

/// The tooltip of `part` in the path's bar and legend: its time, and what
/// it went to.
pub(crate) fn part_account(part: Part, path: &CriticalPath) -> String {
    format!(
        "{}\n{}",
        part_tip(part, path.time_in(part), path.wall(), OF_THE_WALL_TIME),
        explanation(part, path)
    )
}

/// A step's tooltip: everything known of what it ran. `logged` says whether
/// an execution log of the invocation was given.
pub(crate) fn step_tip(step: &Step, logged: bool) -> String {
    let mut lines = vec![step.name().to_owned()];
    let known = [
        ("Mnemonic", step.mnemonic()),
        ("Target", step.target()),
        ("Output", step.primary_output()),
    ];
    lines.extend(
        known
            .into_iter()
            .filter_map(|(key, value)| Some(format!("{key}: {}", value?))),
    );
    lines.extend(work_note(step.work(), logged));
    lines.join("\n")
}

/// What a step's Could save cell shows: its ceiling, nothing where the
/// model holds no action of the step.
pub(crate) fn could_save(step: &Step) -> String {
    step.model()
        .map_or(String::new(), |model| duration(model.ceiling))
}

/// The tooltip of a step's Could save cell, `None` where it shows nothing.
pub(crate) fn could_save_tip(step: &Step) -> Option<String> {
    let StepModel {
        duration: modeled,
        ceiling,
    } = step.model()?;
    if modeled.is_zero() {
        return Some(
            "No action span, as for a cache hit, so the model counts it as zero.".to_owned(),
        );
    }
    let mut tip = format!(
        "Taking no time, it would end the modeled build {} sooner.",
        duration(ceiling)
    );
    let (modeled, ceiling) = (duration(modeled), duration(ceiling));
    // A ceiling that reads the same as the duration shows the reader no bound.
    if ceiling != modeled {
        tip.push_str(&format!(
            "\nModeled at {modeled}; another chain caps the saving at {ceiling}."
        ));
    }
    Some(tip)
}

/// The modeled build against the time the observed path spent in action
/// spans, the only time on it the model holds.
pub(crate) fn modeled_line(path: &CriticalPath, modeled: &Modeled) -> String {
    format!(
        "Longest dependency chain: {} (critical path's actions: {})",
        duration(modeled.length()),
        duration(path.time_in(Part::Action))
    )
}

/// What the model assumes and how to read it against the observed path,
/// and where it holds too little of the build or the path to go by, what.
pub(crate) fn modeled_tip(path: &CriticalPath, modeled: &Modeled) -> String {
    let mut tip = format!(
        "If every action started once its inputs were ready, the build would take {}.\n\
         The critical path's actions took {}.\n\
         Match: the model found Bazel's chain, and \"Could save\" holds.\n\
         Well below: parallelism or scheduling held the build back.\n\
         Graph from aquery, times from profile; untimed actions (mostly cache hits) \
         count as zero.",
        duration(modeled.length()),
        duration(path.time_in(Part::Action)),
    );
    let steps = path.steps().len();
    let unmodeled = path
        .steps()
        .iter()
        .filter(|step| step.model().is_none())
        .count();
    if fraction_of(modeled.timed(), modeled.spanned()) < FEW_MATCHED {
        tip.push_str(&format!(
            "\nOnly {} of {} actions matched aquery by output path; aquery likely ran with \
             other targets or flags.",
            modeled.timed(),
            modeled.spanned(),
        ));
    } else if unmodeled > 0 {
        tip.push_str(&format!(
            "\n{unmodeled} of {steps} steps not in the model; their \"Could save\" is empty."
        ));
    }
    tip
}

/// `part` of `whole`, and of nothing all of it.
fn fraction_of(part: usize, whole: usize) -> f64 {
    match whole {
        0 => 1.0,
        _ => part as f64 / whole as f64,
    }
}

/// The title of the list of a path's steps, beside that of the actions
/// nearly critical.
pub(crate) const STEPS_TITLE: &str = "Steps";

/// The tooltip of the choice of the list of a path's steps.
pub(crate) const STEPS_TIP: &str = "Critical path steps";

/// The tooltip of the choice of the list of actions nearly critical.
pub(crate) const NEARLY_CRITICAL_TIP: &str = "Nearly critical actions";

/// The title of the list of actions nearly critical.
pub(crate) fn nearly_critical_title(modeled: &Modeled) -> String {
    format!("Nearly critical ({})", modeled.nearly_critical().len())
}

/// What stands in the list of actions nearly critical where it holds none.
pub(crate) const NO_NEARLY_CRITICAL: &str = "No action off the path can turn critical.";

/// An action nearly critical's tooltip: everything known of it, and its
/// slack.
pub(crate) fn nearly_critical_tip(action: &NearlyCritical) -> String {
    let mut lines = vec![action.description.clone()];
    let known = [
        ("Mnemonic", action.mnemonic.as_deref()),
        ("Target", action.target.as_deref()),
        ("Output", Some(action.primary_output.as_str())),
    ];
    lines.extend(
        known
            .into_iter()
            .filter_map(|(key, value)| Some(format!("{key}: {}", value?))),
    );
    lines.push(format!(
        "Took {}; {} of slack before it delays the modeled build.",
        duration(action.duration),
        duration(action.slack)
    ));
    lines.join("\n")
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use bviz_invocation::ActionGraph;
    use bviz_trace_profile::Profile;

    /// A profile of 10 ms on its main thread whose critical path waits 1 ms,
    /// runs one step of 2 ms, waits 6 ms and runs one more of 0.4 ms, the
    /// profile holding no span of either.
    fn path() -> CriticalPath {
        let json = r#"{"traceEvents":[
            {"name":"thread_name","ph":"M","tid":0,"args":{"name":"Critical Path"}},
            {"name":"thread_name","ph":"M","tid":1,"args":{"name":"Main Thread"}},
            {"cat":"general information","name":"buildTargets","ph":"X","ts":0,"dur":10000,"tid":1},
            {"cat":"critical path component","name":"action 'Compiling a.c'","ph":"X","ts":1000,"dur":2000,"tid":0},
            {"cat":"critical path component","name":"action 'Linking a'","ph":"X","ts":9000,"dur":400,"tid":0}
        ]}"#;
        CriticalPath::of(
            &Profile::from_bytes(json.as_bytes()).expect("parse"),
            None,
            None,
        )
        .expect("path")
    }

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    #[test]
    fn the_headline_sums_the_path_against_the_wall_time() {
        assert_eq!(
            headline(&path()),
            "Wall time 10.00ms · critical path 2.40ms (24.0% of it) over 2 steps"
        );
    }

    #[test]
    fn the_headline_says_how_much_of_the_path_a_given_log_accounts_for() {
        let json = r#"{"traceEvents":[
            {"name":"thread_name","ph":"M","tid":0,"args":{"name":"Critical Path"}},
            {"name":"thread_name","ph":"M","tid":1,"args":{"name":"Main Thread"}},
            {"cat":"general information","name":"buildTargets","ph":"X","ts":0,"dur":10000,"tid":1},
            {"cat":"critical path component","name":"action 'Linking a'","ph":"X","ts":9000,"dur":400,"tid":0}
        ]}"#;
        let profile = Profile::from_bytes(json.as_bytes()).expect("parse");
        let unrelated = bviz_invocation::spawn::LoggedSpawns::default();
        let path = CriticalPath::of(&profile, Some(&unrelated), None).expect("path");
        assert_eq!(
            headline(&path),
            "Wall time 10.00ms · critical path 400µs (4.0% of it) over 1 step · \
             spawns logged for 0.0% of it"
        );
    }

    /// A path of three steps: one whose span's output a remote spawn lists,
    /// one with a span no spawn is logged of, and one with neither.
    fn logged_path(logged: bool) -> CriticalPath {
        use bviz_invocation::spawn::{Builder, Logged, PhaseTiming, REPORTED_PHASES};
        let json = r#"{"traceEvents":[
            {"name":"thread_name","ph":"M","tid":0,"args":{"name":"Critical Path"}},
            {"name":"thread_name","ph":"M","tid":1,"args":{"name":"Main Thread"}},
            {"name":"thread_name","ph":"M","tid":2,"args":{"name":"skyframe-evaluator-execution-0"}},
            {"cat":"general information","name":"buildTargets","ph":"X","ts":0,"dur":10000,"tid":1},
            {"cat":"critical path component","name":"action 'Compiling a.c'","ph":"X","ts":1000,"dur":2000,"tid":0},
            {"cat":"critical path component","name":"action 'Writing file b'","ph":"X","ts":3000,"dur":100,"tid":0},
            {"cat":"critical path component","name":"action 'Linking a'","ph":"X","ts":4000,"dur":400,"tid":0},
            {"cat":"action processing","name":"Compiling a.c","ph":"X","ts":1001,"dur":2000,"tid":2,"out":"bazel-out/a.o"},
            {"cat":"action processing","name":"Writing file b","ph":"X","ts":3001,"dur":99,"tid":2}
        ]}"#;
        let profile = Profile::from_bytes(json.as_bytes()).expect("parse");
        let mut builder = Builder::with_capacity(1, 1, 0);
        builder.output("bazel-out/a.o");
        builder.push(Logged {
            runner: "remote",
            mnemonic: "CppCompile",
            target_label: "//a",
            cache_hit: false,
            timing: PhaseTiming::new(ms(3), [Duration::ZERO; REPORTED_PHASES]),
        });
        let log = builder.finish();
        CriticalPath::of(&profile, logged.then_some(&log), None).expect("path")
    }

    #[test]
    fn a_step_is_tagged_by_what_ran_it_as_far_as_the_log_tells() {
        let tags = |logged| -> Vec<Option<&str>> {
            logged_path(logged)
                .steps()
                .iter()
                .map(|step| work_tag(step.work(), logged))
                .collect()
        };
        assert_eq!(
            tags(true),
            [Some("remote"), Some("no spawn"), Some("no span")]
        );
        assert_eq!(tags(false), [None, None, Some("no span")]);
    }

    #[test]
    fn spawns_adding_up_past_their_action_say_they_were_scaled() {
        let path = logged_path(true);
        let note = work_note(path.steps()[0].work(), true).expect("a note");
        assert_eq!(
            note,
            "Ran remotely: 1 spawn, 3.00ms.\nPhases scaled down to the action's time."
        );
    }

    /// The primary outputs of the actions of [`modeled_path`]'s graph.
    pub(in crate::critical_path) const MODELED_OUTPUTS: [&str; 5] = [
        "bazel-out/a.h",
        "bazel-out/a.o",
        "bazel-out/b.o",
        "bazel-out/a",
        "bazel-out/c",
    ];
    /// Each pair of positions in [`MODELED_OUTPUTS`] of an action and one it
    /// takes an input from.
    const MODELED_DEPENDENCIES: [(usize, usize); 4] = [(1, 0), (3, 1), (3, 2), (4, 3)];

    /// A path of a spanned compile and link and two unspanned steps after a
    /// wait, the first naming its output and the second none, modeled from
    /// the actions of `outputs`: the compile's 2 ms takes a generated
    /// header's 0.3 ms before it, and is bounded by a 1.5 ms compile beside
    /// them.
    pub(in crate::critical_path) fn modeled_path(outputs: &[&str]) -> CriticalPath {
        let json = r#"{"traceEvents":[
            {"name":"thread_name","ph":"M","tid":0,"args":{"name":"Critical Path"}},
            {"name":"thread_name","ph":"M","tid":1,"args":{"name":"Main Thread"}},
            {"name":"thread_name","ph":"M","tid":2,"args":{"name":"skyframe-evaluator-execution-0"}},
            {"name":"thread_name","ph":"M","tid":3,"args":{"name":"skyframe-evaluator-execution-1"}},
            {"cat":"general information","name":"buildTargets","ph":"X","ts":0,"dur":10000,"tid":1},
            {"cat":"critical path component","name":"action 'Compiling a.c'","ph":"X","ts":1000,"dur":2000,"tid":0},
            {"cat":"critical path component","name":"action 'Linking a'","ph":"X","ts":3600,"dur":400,"tid":0},
            {"cat":"critical path component","name":"action 'Writing c'","ph":"X","ts":4000,"dur":100,"tid":0,"out":"bazel-out/c"},
            {"cat":"critical path component","name":"action 'Checking d'","ph":"X","ts":4200,"dur":100,"tid":0},
            {"cat":"action processing","name":"Generating a.h","ph":"X","ts":500,"dur":300,"tid":3,"out":"bazel-out/a.h"},
            {"cat":"action processing","name":"Compiling a.c","ph":"X","ts":1000,"dur":2000,"tid":2,"out":"bazel-out/a.o"},
            {"cat":"action processing","name":"Compiling b.c","ph":"X","ts":1000,"dur":1500,"tid":3,"out":"bazel-out/b.o","args":{"mnemonic":"CppCompile","target":"//b"}},
            {"cat":"action processing","name":"Linking a","ph":"X","ts":3600,"dur":400,"tid":2,"out":"bazel-out/a"}
        ]}"#;
        let profile = Profile::from_bytes(json.as_bytes()).expect("parse");
        let position = |out: &str| outputs.iter().position(|&kept| kept == out);
        let dependencies = MODELED_DEPENDENCIES
            .iter()
            .filter_map(|&(dependent, dependency)| {
                Some((
                    position(MODELED_OUTPUTS[dependent])?,
                    position(MODELED_OUTPUTS[dependency])?,
                ))
            })
            .collect();
        let graph = ActionGraph::new(
            outputs.iter().map(|&out| Some(out.to_owned())).collect(),
            dependencies,
        );
        CriticalPath::of(&profile, None, Some(&graph)).expect("path")
    }

    #[test]
    fn a_step_says_what_it_could_save_and_when_another_chain_bounds_it() {
        let modeled = modeled_path(&MODELED_OUTPUTS);
        let [compile, link, written, checked] = modeled.steps() else {
            panic!("four steps");
        };
        assert_eq!(could_save(compile), "800µs");
        assert_eq!(
            could_save_tip(compile).as_deref(),
            Some(
                "Taking no time, it would end the modeled build 800µs sooner.\n\
                 Modeled at 2.00ms; another chain caps the saving at 800µs."
            )
        );
        assert_eq!(
            could_save_tip(link).as_deref(),
            Some("Taking no time, it would end the modeled build 400µs sooner.")
        );
        assert_eq!(could_save(written), "0µs");
        assert!(could_save_tip(written).is_some_and(|tip| tip.starts_with("No action span")));
        assert_eq!(could_save(checked), "");
        assert_eq!(could_save_tip(checked), None);
    }

    #[test]
    fn the_model_is_held_against_the_observed_path_s_actions() {
        let path = modeled_path(&MODELED_OUTPUTS);
        let modeled = path.modeled().expect("modeled");
        assert_eq!(
            modeled_line(&path, modeled),
            "Longest dependency chain: 2.70ms (critical path's actions: 2.40ms)"
        );
        let tip = modeled_tip(&path, modeled);
        assert!(
            tip.starts_with(
                "If every action started once its inputs were ready, the build would take \
                 2.70ms.\n"
            ),
            "{tip}"
        );
        assert!(
            tip.contains("\nThe critical path's actions took 2.40ms.\n"),
            "{tip}"
        );
        assert!(
            tip.ends_with("\n1 of 4 steps not in the model; their \"Could save\" is empty."),
            "{tip}"
        );
    }

    #[test]
    fn a_model_matching_few_spans_says_how_they_are_matched() {
        let path = modeled_path(&["bazel-out/a.o"]);
        let tip = modeled_tip(&path, path.modeled().expect("modeled"));
        assert!(
            tip.ends_with(
                "\nOnly 1 of 4 actions matched aquery by output path; aquery likely ran with \
                 other targets or flags."
            ),
            "{tip}"
        );
    }

    #[test]
    fn an_action_nearly_critical_says_what_it_is_and_its_slack() {
        let path = modeled_path(&MODELED_OUTPUTS);
        let modeled = path.modeled().expect("modeled");
        assert_eq!(nearly_critical_title(modeled), "Nearly critical (2)");
        assert_eq!(
            nearly_critical_tip(&modeled.nearly_critical()[1]),
            "Compiling b.c\nMnemonic: CppCompile\nTarget: //b\nOutput: bazel-out/b.o\n\
             Took 1.50ms; 800µs of slack before it delays the modeled build."
        );
    }

    #[test]
    fn folded_phases_are_listed_together() {
        assert_eq!(folded(&["parse"]), "parse");
        assert_eq!(
            folded(&["parse", "network", "fetch"]),
            "parse, network and fetch"
        );
    }

    #[test]
    fn a_lever_says_its_time_and_what_pulls_it() {
        assert_eq!(
            lever(Lever::Fetch, ms(1500), None),
            "downloads 1.50s → --remote_download_minimal"
        );
    }

    #[test]
    fn a_lever_says_what_pulling_it_saves_only_where_the_model_bounds_it_below_its_time() {
        assert_eq!(lever_bound(None, ms(1500)), None);
        assert_eq!(lever_bound(Some(ms(1500)), ms(1500)), None);
        let bound = lever_bound(Some(ms(400)), ms(1500));
        assert_eq!(bound, Some(ms(400)));
        assert_eq!(
            lever(Lever::Execution, ms(1500), bound),
            "execution 1.50s, saves at most 400.00ms → a faster tool, or split the target"
        );
        assert!(
            lever_account(Lever::Execution, bound)
                .ends_with("\nSaves at most 400.00ms: other chains then bound the build.")
        );
        assert_eq!(
            lever_account(Lever::Execution, None),
            lever_tip(Lever::Execution)
        );
    }

    #[test]
    fn the_wait_s_account_names_the_longest_wait() {
        assert_eq!(
            part_account(Part::Waiting, &path()),
            "waiting: 6.00ms (60.0% of the wall time)\n\
             Steps waiting for inputs off the path, input checks, or a free --jobs slot.\n\
             Longest: 6.00ms before Linking a."
        );
    }

    #[test]
    fn a_part_s_account_opens_with_its_time_then_says_what_it_went_to() {
        assert_eq!(
            part_account(Part::Tail, &path()),
            "tail: 600µs (6.0% of the wall time)\n\
             After the last step, until the command ended."
        );
    }

    #[test]
    fn the_first_step_s_wait_is_the_lead_in_and_shows_nothing() {
        assert_eq!(waited(0, Duration::ZERO), "");
        assert_eq!(waited(1, Duration::ZERO), NO_WAIT);
        assert_eq!(waited(1, ms(6)), "6.00ms");
    }
}
