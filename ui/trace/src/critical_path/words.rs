//! How the parts of a critical path read on screen.

use crate::chart::format_duration;
use bviz_critical_path::{
    Coverage, CriticalPath, Fit, Lever, Part, Phase, Ran, SpanSplit, SpawnRunner, Spawned, Step,
    Work,
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
        SpawnRunner::RemoteExecution => "run remotely",
        SpawnRunner::Worker => "run on a persistent worker",
        SpawnRunner::Sandboxed => "run in a local sandbox",
        SpawnRunner::Local => "run locally",
        SpawnRunner::RemoteCacheHit => "answered by the remote cache",
        SpawnRunner::DiskCacheHit => "answered by the disk cache",
        SpawnRunner::Deduplicated => "answered by the outputs of an identical spawn",
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
            "An action span but no logged spawn: the action ran none, as writing a file \
             or a symlink does, or its spawn was not logged."
        }
        (Work::Unspanned(_), false) => "No action span: an action-cache hit, or not recorded.",
        (Work::Unspanned(_), true) => {
            "Neither an action span nor a logged spawn: an action-cache hit, or not recorded."
        }
        (Work::ChangePruned, _) => "Change pruned: its inputs were unchanged, so it did not run.",
    };
    Some(note.to_owned())
}

/// What the execution log says of a step's spawns.
fn spawned_note(spawned: &Spawned) -> String {
    let spawns = spawned.spawns();
    let mut note = format!(
        "Its action was {}: {spawns} {}, {} as Bazel measured {}.",
        runner_phrase(spawned.runner()),
        if spawns == 1 { "spawn" } else { "spawns" },
        duration(spawned.logged()),
        if spawns == 1 { "it" } else { "them" },
    );
    if spawned.fit() == Fit::Scaled {
        note.push_str(
            " Their phases add up past the action's time, so they are scaled to fill it.",
        );
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
        "Logged actions on the path by spawn phase — {steps} {}, {}",
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

/// A lever with the time on the path it acts on, and what pulls it.
pub(crate) fn lever(lever: Lever, time: Duration) -> String {
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
        Lever::CacheHitOverhead => ("steps with neither span nor spawn", "output checking cost"),
    };
    format!("{name} {} → {pull}", duration(time))
}

/// What the time a lever acts on is, and how pulling it shortens the path.
pub(crate) fn lever_tip(lever: Lever) -> &'static str {
    match lever {
        Lever::LeadIn => {
            "Launching Bazel, loading, analysis and fetching repositories before the first \
             step: a server kept running keeps its analysis cache, and repositories fetched \
             or vendored ahead skip the fetch."
        }
        Lever::Waiting => {
            "Steps waiting after the step before them finished: for inputs built off the \
             path, or a free job slot. Cutting dependencies off the path or raising --jobs \
             lets them start sooner."
        }
        Lever::RemoteQueue => {
            "Spawns queued for a remote executor before a worker took them: more executor \
             capacity, or fewer builds sharing it, shortens the queue."
        }
        Lever::LocalQueue => {
            "Spawns waiting for Bazel's local resource manager to grant the CPU and memory \
             they declare: more local resources (--local_resources), or actions declaring \
             less, let more run at once."
        }
        Lever::Parse => {
            "Spawns building their remote execution requests, mostly computing the input \
             Merkle tree: fewer inputs per action build faster. Bazel 9.0 and later keep \
             the trees of runfiles and directories across builds; earlier releases cache \
             trees within a build under --experimental_remote_merkle_tree_cache."
        }
        Lever::Upload => {
            "Spawns uploading inputs the remote cache lacked, beyond the time in calls to \
             it: inputs built locally or not yet cached. Building those remotely too, or \
             caching them, leaves less to upload."
        }
        Lever::Network => {
            "Wall time with a call to the remote cache outstanding: action-cache lookups, \
             checks for missing inputs, and the bytes of uploads and downloads. Lower \
             latency to the cache, and fewer, smaller inputs per action, shorten it."
        }
        Lever::RemoteSetup => {
            "Remote executors staging spawns' inputs between taking a spawn and running \
             it: fewer or smaller inputs per action, or an executor caching inputs between \
             actions, shortens it."
        }
        Lever::LocalSetup => {
            "Bazel fetching spawns' inputs and setting up their sandboxes or workers: \
             persistent workers, a cheaper sandbox, or fewer inputs to fetch shorten it."
        }
        Lever::Execution => {
            "Spawns running their commands: a faster tool, or a target split so that less \
             of it rebuilds and its parts run side by side."
        }
        Lever::ProcessOutputs => {
            "Spawns' outputs moved out of where they ran: a remote executor uploading them \
             to its cache, a local sandbox or worker copying them into place. Fewer, \
             smaller outputs move faster."
        }
        Lever::Fetch => {
            "Downloading spawns' outputs, beyond the time in calls to the cache: \
             --remote_download_toplevel (the default from Bazel 7.0) or \
             --remote_download_minimal fetches only the outputs needed locally."
        }
        Lever::LocalOnRemote => {
            "Steps executed locally in a build whose execution log shows spawns executed \
             remotely: a strategy or a no-remote tag may be keeping them off the remote \
             executor."
        }
        Lever::CacheHitOverhead => {
            "Steps with neither an action span nor a logged spawn, likely action-cache \
             hits, that still took time: Bazel checking their inputs and outputs, which \
             output-checking and digest settings bear on."
        }
    }
}

/// What an action's time in `phase` went to.
fn phase_explanation(phase: Phase) -> &'static str {
    match phase {
        Phase::Parse => {
            "Building spawns' remote execution requests, mostly their input Merkle trees."
        }
        Phase::Upload => {
            "Uploading inputs the remote cache lacked, beyond the time in calls to the cache."
        }
        Phase::Network => {
            "Calls to the remote cache outstanding: action-cache lookups, checks for missing \
             inputs, and the bytes of uploads and downloads."
        }
        Phase::Queue => {
            "Waiting for a remote executor to start the spawn, or for the local resources \
             to run it."
        }
        Phase::Setup => {
            "Preparing to run: a remote executor staging inputs, or Bazel fetching inputs \
             and setting up a local sandbox or worker."
        }
        Phase::Execute => "Running the spawns' commands.",
        Phase::ProcessOutputs => {
            "Moving outputs out: a remote executor uploading them to its cache, a local \
             sandbox or worker copying them into place."
        }
        Phase::Fetch => "Downloading outputs, beyond the time in calls to the cache.",
        Phase::Retry => "Earlier attempts of spawns that were run again.",
        Phase::Other => {
            "Spawn time Bazel's metrics put in no phase, such as waiting on a remote \
             execution beyond what the executor reports."
        }
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
        "{}\nThe actions' time outside their spawns' metrics: deleting stale outputs, \
         looking a spawn up in the cache when it misses, storing a locally run spawn's \
         result in the cache, writing the execution log, and checking the outputs.",
        named_tip(
            AROUND_SPAWNS,
            coverage.around(),
            coverage.action(),
            OF_THE_ACTIONS
        )
    )
}

/// What the wall time in `part` went to, and what tends to shorten it.
fn explanation(part: Part, path: &CriticalPath) -> String {
    match part {
        Part::LeadIn => "launching Bazel, loading, analysis and fetching repositories, before \
                         the first step started; the timeline's phase markers tell them apart."
            .to_owned(),
        Part::Waiting => {
            let longest = path.longest_wait().map_or(String::new(), |step| {
                format!(
                    " The longest wait, {}, was before {}.",
                    duration(step.gap()),
                    step.name()
                )
            });
            format!(
                "steps waiting after the step before them finished: for their inputs off \
                 the path, for input checks, or for a free --jobs slot.{longest}"
            )
        }
        Part::BeforeAction => {
            "steps before their action spans started: input discovery and action-cache checks."
                .to_owned()
        }
        Part::Action => {
            "the actions themselves; a faster, cached or removed action on the path shortens it."
                .to_owned()
        }
        Part::Unspanned => "steps the profile holds no action span of: action-cache hits, or \
                            spans it left out."
            .to_owned(),
        Part::Tail => "after the last step finished, until Bazel ended the command.".to_owned(),
    }
}

/// The tooltip of `part` in the path's bar and legend: its time, and what
/// it went to.
pub(crate) fn part_account(part: Part, path: &CriticalPath) -> String {
    let time = path.time_in(part);
    let mut said = explanation(part, path);
    if let Some(first) = said.get(..1) {
        said.replace_range(..1, &first.to_uppercase());
    }
    format!(
        "{}\n{said}",
        part_tip(part, time, path.wall(), OF_THE_WALL_TIME)
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

#[cfg(test)]
mod tests {
    use super::*;
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
        CriticalPath::of(&Profile::from_bytes(json.as_bytes()).expect("parse"), None).expect("path")
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
        let path = CriticalPath::of(&profile, Some(&unrelated)).expect("path");
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
        CriticalPath::of(&profile, logged.then_some(&log)).expect("path")
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
            "Its action was run remotely: 1 spawn, 3.00ms as Bazel measured it. Their \
             phases add up past the action's time, so they are scaled to fill it."
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
            lever(Lever::Fetch, ms(1500)),
            "downloads 1.50s → --remote_download_minimal"
        );
    }

    #[test]
    fn the_wait_s_account_names_the_longest_wait() {
        assert_eq!(
            part_account(Part::Waiting, &path()),
            "waiting: 6.00ms (60.0% of the wall time)\n\
             Steps waiting after the step before them finished: for their inputs off \
             the path, for input checks, or for a free --jobs slot. The longest wait, \
             6.00ms, was before Linking a."
        );
    }

    #[test]
    fn a_part_s_account_opens_with_its_time_then_says_what_it_went_to() {
        assert_eq!(
            part_account(Part::Tail, &path()),
            "tail: 600µs (6.0% of the wall time)\n\
             After the last step finished, until Bazel ended the command."
        );
    }

    #[test]
    fn the_first_step_s_wait_is_the_lead_in_and_shows_nothing() {
        assert_eq!(waited(0, Duration::ZERO), "");
        assert_eq!(waited(1, Duration::ZERO), NO_WAIT);
        assert_eq!(waited(1, ms(6)), "6.00ms");
    }
}
