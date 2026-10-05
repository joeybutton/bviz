//! How the parts of a critical path read on screen.

use crate::chart::format_duration;
use bviz_critical_path::{Coverage, CriticalPath, Part, Phase, Step, Work};
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

/// A word on what the profile says a step did, where its span leaves
/// something to say.
pub(crate) fn work_tag(work: Work) -> Option<&'static str> {
    match work {
        Work::Spanned(_) => None,
        Work::Unspanned(_) => Some("no span"),
        Work::ChangePruned => Some("pruned"),
    }
}

/// What [`work_tag`] stands for.
fn work_note(work: Work) -> Option<&'static str> {
    match work {
        Work::Spanned(_) => None,
        Work::Unspanned(_) => Some("No action span: an action-cache hit, or not recorded."),
        Work::ChangePruned => Some("Change pruned: its inputs were unchanged, so it did not run."),
    }
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

/// A step's tooltip: everything known of what it ran.
pub(crate) fn step_tip(step: &Step) -> String {
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
    lines.extend(work_note(step.work()).map(str::to_owned));
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

    #[test]
    fn folded_phases_are_listed_together() {
        assert_eq!(folded(&["parse"]), "parse");
        assert_eq!(
            folded(&["parse", "network", "fetch"]),
            "parse, network and fetch"
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
