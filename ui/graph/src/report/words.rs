//! The words the card reads what a build reported in.

use super::{
    Action, Badged, BuildTally, Causes, Ended, Fate, Listed, Mismatches, Name, Row, Said, Saying,
    Share, Shown, TestTally, Traced, Unbuilt, wholly_cached,
};
use crate::duration::lasted;
use crate::status::Status;
use bviz_invocation::{Configurations, ConfiguredTarget, TestOutcome, TestStatus};

/// `count` of a thing named `one`, or `many` where there is not one of it.
fn counted(count: usize, one: &str, many: &str) -> String {
    match count {
        1 => format!("1 {one}"),
        count => format!("{count} {many}"),
    }
}

/// `total` things of which each of `parts` counts some: the total, and after
/// it each count that is not zero.
fn tallied(total: String, parts: &[(usize, &str)]) -> String {
    let counts: Vec<String> = parts
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, name)| format!("{count} {name}"))
        .collect();
    match counts.is_empty() {
        true => total,
        false => format!("{total}: {}", counts.join(", ")),
    }
}

impl BuildTally {
    pub(super) fn line(&self) -> String {
        tallied(
            counted(self.total(), "target", "targets"),
            &[
                (self.built, "built"),
                (self.failed, "failed"),
                (self.not_built, "not built"),
                (self.skipped, "skipped"),
            ],
        )
    }
}

impl TestTally {
    /// `None` for a build that ran no test.
    pub(super) fn line(&self) -> Option<String> {
        let passed = match self.cached {
            0 => "passed".to_owned(),
            cached => format!("passed ({cached} cached)"),
        };
        (self.total() > 0).then(|| {
            tallied(
                counted(self.total(), "test", "tests"),
                &[
                    (self.passed, &passed),
                    (self.flaky, "flaky"),
                    (self.failed, "failed"),
                    (self.timed_out, "timed out"),
                    (self.failed_remotely, "failed remotely"),
                    (self.incomplete, "incomplete"),
                    (self.not_run, "not run"),
                    (self.not_built, "not built"),
                ],
            )
        })
    }
}

/// What `test`'s runs and shards add to the account of it, which counts
/// attempts within one shard of one run and times all of them together.
/// Empty for a test run once and in one piece.
fn spread(test: &TestOutcome) -> String {
    let runs = counted(test.runs as usize, "run", "runs");
    let shards = test
        .shards
        .map(|shards| counted(shards.get() as usize, "shard", "shards"));
    match (test.runs > 1, shards) {
        (false, None) => String::new(),
        (false, Some(shards)) => format!(" over {shards}"),
        (true, None) => format!(" over {runs}"),
        (true, Some(shards)) => format!(" over {runs} of {shards}"),
    }
}

/// How running `test` ended, in words.
fn ran(test: &TestOutcome) -> String {
    let attempts = test.attempts;
    let every_attempt = match attempts {
        0 | 1 => String::new(),
        attempts => format!(" on each of {attempts} attempts"),
    };
    let took = lasted(test.run_duration);
    let spread = spread(test);
    match test.status {
        TestStatus::Passed => match wholly_cached(test) {
            true => format!("passed (cached){spread}"),
            false => format!("passed in {took}{spread}"),
        },
        TestStatus::Flaky => format!("flaky: passed on attempt {attempts} in {took}{spread}"),
        TestStatus::Failed => format!("failed{every_attempt} in {took}{spread}"),
        TestStatus::Timeout => format!("timed out{every_attempt} after {took}{spread}"),
        TestStatus::RemoteFailure => format!("failed remotely{every_attempt}{spread}"),
        TestStatus::Incomplete => format!("incomplete: not every run ended{spread}"),
        TestStatus::NoStatus => "no status: the test did not run".to_owned(),
        TestStatus::ToolHaltedBeforeTesting => "not run: the build ended before testing".to_owned(),
        TestStatus::FailedToBuild => "not run: the test could not be built".to_owned(),
    }
}

/// What a row adds to its account of a thing the graph holds no node for.
const OFF_GRAPH: &str = "not in this graph";

impl Share {
    /// That `count` of the things reported, which `one` names one of and
    /// `many` several, stand as `how` says; `None` where none does.
    fn line(&self, count: usize, one: &str, many: &str, how: &str) -> Option<String> {
        let verb = match count {
            0 => return None,
            1 => "is",
            _ => "are",
        };
        let reported = counted(self.reported, one, many);
        Some(format!("{count} of {reported} {verb} {how}"))
    }

    /// That some of the things reported are not in the graph; `None` where
    /// all are.
    fn off(&self, one: &str, many: &str) -> Option<String> {
        self.line(self.off, one, many, OFF_GRAPH)
    }
}

impl Mismatches {
    /// A line for each way the graph does not hold what the build reports
    /// on.
    pub(super) fn lines(&self) -> impl Iterator<Item = String> {
        let (target, many) = ("reported target", "reported targets");
        let reconfigured = "in this graph under other configurations only";
        [
            self.targets.off(target, many),
            self.targets
                .line(self.reconfigured, target, many, reconfigured),
            self.failed_actions.off("failed action", "failed actions"),
        ]
        .into_iter()
        .flatten()
    }
}

/// Columns a row's hover breaks what the build said at: few enough that the
/// tooltip's own wrapping leaves each line whole, so that [`HOVER_LINES`]
/// bounds its height.
const HOVER_COLUMNS: usize = 64;

/// Lines of what the build said a row's hover shows before it counts the
/// rest: a tooltip taller than its window is cut off by it.
const HOVER_LINES: usize = 32;

/// `line` broken wherever it runs longer than `columns`: at the last space
/// within them, or at `columns` where it holds none.
fn broken(mut line: &str, columns: usize) -> impl Iterator<Item = &str> {
    let mut rest = Some(());
    std::iter::from_fn(move || {
        rest?;
        let Some((cut, beyond)) = line.char_indices().nth(columns) else {
            rest = None;
            return Some(line);
        };
        let (head, tail) = match line[..cut + beyond.len_utf8()].rfind(' ') {
            Some(space) if space > 0 => (&line[..space], &line[space + 1..]),
            _ => line.split_at(cut),
        };
        line = tail;
        Some(head)
    })
}

/// The lines of `text`, each [`broken`] at `columns`.
fn wrapped(text: &str, columns: usize) -> impl Iterator<Item = &str> {
    text.lines().flat_map(move |line| broken(line, columns))
}

/// What a row's hover reads: its two lines whole, over what the build said of
/// the failure.
fn hover(name: &str, detail: &str, message: &str) -> String {
    let mut said = wrapped(message.trim(), HOVER_COLUMNS).peekable();
    let mut text = format!("{name}\n{detail}");
    if said.peek().is_some() {
        text.push('\n');
    }
    for line in said.by_ref().take(HOVER_LINES) {
        text.push('\n');
        text.push_str(line);
    }
    let beyond = said.count();
    if beyond > 0 {
        text.push_str(&format!(
            "\n… and {}",
            counted(beyond, "more line", "more lines")
        ));
    }
    text
}

/// What kind of action `mnemonic` says one is, where it says any.
fn kind(mnemonic: &str) -> &str {
    match mnemonic.is_empty() {
        true => "action",
        false => mnemonic,
    }
}

impl<'a> Name<'a> {
    /// What kind of thing [`subject`](Self::subject) names, where the
    /// subject alone does not say.
    fn lead(self) -> Option<String> {
        match self {
            Name::Owned { mnemonic, .. } => Some(kind(mnemonic).to_owned()),
            Name::Writing { mnemonic, .. } => Some(format!("{} writing", kind(mnemonic))),
            Name::Label(_) => None,
        }
    }

    /// The label or path naming the thing.
    fn subject(self) -> &'a str {
        match self {
            Name::Owned { owner: subject, .. }
            | Name::Writing {
                primary_output: subject,
                ..
            }
            | Name::Label(subject) => subject,
        }
    }
}

/// How `action` failed: by the code its process exited with, where the build
/// states one.
fn exited(action: &Action) -> String {
    match action.exit_code {
        Some(code) => format!("exited {code}"),
        None => "failed".to_owned(),
    }
}

/// That a root cause did `what`, and failed `failing` targets where it
/// failed any.
fn blamed(what: &str, failing: usize) -> String {
    match failing {
        0 => what.to_owned(),
        failing => format!("{what}, failing {}", counted(failing, "target", "targets")),
    }
}

impl Listed {
    /// How the thing listed came to be listed.
    fn account(&self) -> String {
        match self {
            Listed::Action {
                action, failing, ..
            } => blamed(&exited(action), *failing),
            Listed::Target { came, failing, .. } => {
                let what = match came {
                    Unbuilt::Unanalysed => "could not be analysed",
                    Unbuilt::Uncaused => "failed, and the build names no cause",
                    Unbuilt::Unreported => "could not be loaded or analysed",
                };
                blamed(what, *failing)
            }
            Listed::Test { test, .. } => ran(test),
        }
    }

    /// What the card says of the node of the thing listed, in the words of
    /// its row. What the build said of the thing is left to the row, whose
    /// hover has the room for it.
    fn sentence(&self) -> String {
        let (name, account) = (self.name(), self.account());
        match name.lead() {
            Some(lead) => format!("{lead} {} {account}", name.subject()),
            None => format!("{} {account}", name.subject()),
        }
    }

    /// What the build said of the thing listed.
    fn message(&self) -> &str {
        match self {
            Listed::Action { message, .. } => message,
            Listed::Target { error, .. } => error,
            Listed::Test { .. } => "",
        }
    }
}

impl Row {
    pub(super) fn lead(&self) -> Option<String> {
        self.listed.name().lead()
    }

    /// The row's subject, followed where another row names its thing alike
    /// by the configuration of its own, as `configurations` names it.
    pub(super) fn subject(&self, configurations: &Configurations) -> String {
        let subject = self.listed.name().subject();
        match self.listed.configuration().filter(|_| self.alike) {
            Some(checksum) => {
                let configuration = configurations.name_or_checksum(checksum);
                format!("{subject} ({configuration})")
            }
            None => subject.to_owned(),
        }
    }

    /// The row's account of the thing listed, which says so where the graph
    /// does not hold it.
    fn account(&self) -> String {
        let account = self.listed.account();
        match self.shown {
            Shown::Itself(_) => account,
            Shown::Failed(_) | Shown::Nothing => format!("{account} · {OFF_GRAPH}"),
        }
    }

    /// The row's second line: its account, and the first line of what the
    /// build said.
    pub(super) fn detail(&self) -> String {
        let account = self.account();
        match self.listed.message().trim().lines().next() {
            Some(first) => format!("{account} — {first}"),
            None => account,
        }
    }

    pub(super) fn hover(&self, configurations: &Configurations) -> String {
        let subject = self.subject(configurations);
        let name = match self.lead() {
            Some(lead) => format!("{lead} {subject}"),
            None => subject,
        };
        hover(&name, &self.account(), self.listed.message())
    }
}

impl Traced {
    /// What the cause says of the failure of `failed`.
    fn words(&self, failed: ConfiguredTarget<&str>) -> String {
        match self {
            Traced::Action(action) => {
                let (kind, exited) = (kind(&action.mnemonic), exited(action));
                match action.owner.as_ref().map(ConfiguredTarget::borrowed) {
                    Some(owner) if owner == failed => format!("its {kind} action {exited}"),
                    Some(owner) => format!("{} ({kind}) {exited}", owner.label()),
                    None => format!("the {kind} writing {} {exited}", action.primary_output),
                }
            }
            Traced::Target { label, error } => match error.trim().lines().next() {
                Some(error) if label == failed.label() => error.to_owned(),
                Some(error) => format!("{label} could not be loaded or analysed: {error}"),
                None => format!("{label} could not be loaded or analysed"),
            },
        }
    }
}

impl Fate {
    fn words(&self) -> String {
        let target = self.target.borrowed();
        let label = target.label();
        let failed = |how: &str, causes: &Option<Causes>| match causes {
            Some(Causes { named, more: 0 }) => format!("{label} {how}: {}", named.words(target)),
            Some(Causes { named, more }) => format!(
                "{label} {how}: {}, and {}",
                named.words(target),
                counted(*more, "more cause", "more causes")
            ),
            None => format!("{label} {how}, and the build names no cause"),
        };
        match &self.ended {
            Ended::Built => format!("{label} built"),
            Ended::Skipped => format!("{label} skipped: incompatible with the target platform"),
            Ended::NotBuilt => format!("{label} not built: the build ended first"),
            Ended::Tested(test) => format!("{label} {}", ran(test)),
            Ended::Failed(causes) => failed("failed", causes),
            Ended::Unanalysed(causes) => failed("could not be analysed", causes),
        }
    }
}

impl Said<'_> {
    pub(super) fn words(&self) -> String {
        match self.saying {
            Saying::RootCause(row) => row.listed.sentence(),
            Saying::Target(fate) => fate.words(),
        }
    }
}

/// What the card says of a box that stands at `status` for something it
/// holds.
fn holds(status: Status) -> &'static str {
    match status {
        Status::Skipped => "everything the build reports in this box was skipped",
        Status::Succeeded => "nothing the build reports in this box went wrong",
        Status::Unfinished => "this box holds a target the build did not finish",
        Status::Flaky => "this box holds a flaky test",
        Status::TestFailed => "this box holds a test that did not pass",
        Status::Failed => "this box holds a target that failed through something it depends on",
        Status::RootCause => "this box holds the root cause of a failure",
    }
}

impl Badged<'_> {
    /// What the card says of a box badged `status` for this.
    pub(super) fn words(&self, status: Status) -> String {
        let held = holds(status);
        match self {
            Badged::Reported(said) => said.words(),
            Badged::ReportedAndHeld(said) => format!("{}; {held}", said.words()),
            Badged::Held => held.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::test;
    use super::*;
    use bviz_graph_layout::Id;
    use std::num::NonZeroU32;
    use std::time::Duration;

    #[test]
    fn a_long_line_of_what_the_build_said_is_broken_at_a_space() {
        let wrapped = |text, columns| wrapped(text, columns).collect::<Vec<_>>();
        assert_eq!(wrapped("one two three", 7), ["one two", "three"]);
        assert_eq!(wrapped("one two three", 5), ["one", "two", "three"]);
        assert_eq!(wrapped("onetwothree", 4), ["onet", "woth", "ree"]);
        assert_eq!(wrapped("a\n\nb", 4), ["a", "", "b"]);
        assert_eq!(wrapped("", 4), Vec::<&str>::new());
    }

    /// A line is measured and cut in characters, of which a compiler's
    /// message holds some longer than a byte.
    #[test]
    fn a_line_is_broken_between_characters_however_many_bytes_each_takes() {
        let wrapped = |text, columns| wrapped(text, columns).collect::<Vec<_>>();
        assert_eq!(wrapped("ééééééé", 3), ["ééé", "ééé", "é"]);
        assert_eq!(wrapped("naïve café crème", 6), ["naïve", "café", "crème"]);
        assert_eq!(wrapped("‘a’ → ‘b’", 5), ["‘a’ →", "‘b’"]);

        let said = "é".repeat(HOVER_COLUMNS + 1);
        let text = hover("name", "detail", &said);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[3].chars().count(), HOVER_COLUMNS);
        assert_eq!(lines[4], "é");
    }

    #[test]
    fn a_hover_counts_the_lines_it_leaves_out() {
        let lines = |count: usize| vec!["line"; count].join("\n");
        let within = hover("name", "detail", &lines(HOVER_LINES));
        assert_eq!(within.lines().count(), 3 + HOVER_LINES);
        let beyond = hover("name", "detail", &lines(HOVER_LINES + 2));
        assert_eq!(beyond.lines().count(), 3 + HOVER_LINES + 1);
        assert!(beyond.ends_with("\n… and 2 more lines"), "{beyond}");
        assert_eq!(hover("name", "detail", " \n"), "name\ndetail");
    }

    #[test]
    fn a_test_is_said_to_have_ended_as_its_attempts_runs_and_shards_explain() {
        let flaky = TestOutcome {
            attempts: 2,
            actions: 2,
            run_duration: Duration::from_millis(80),
            ..test(TestStatus::Flaky)
        };
        assert_eq!(ran(&flaky), "flaky: passed on attempt 2 in 80ms");
        let retried = TestOutcome {
            attempts: 3,
            ..test(TestStatus::Failed)
        };
        assert_eq!(ran(&retried), "failed on each of 3 attempts in 0.3s");
        let cached = TestOutcome {
            cached_actions: 1,
            ..test(TestStatus::Passed)
        };
        assert_eq!(ran(&cached), "passed (cached)");
        assert_eq!(ran(&test(TestStatus::Passed)), "passed in 0.3s");

        let sharded = |runs, status| TestOutcome {
            runs,
            shards: NonZeroU32::new(4),
            ..test(status)
        };
        assert_eq!(
            ran(&sharded(1, TestStatus::Failed)),
            "failed in 0.3s over 4 shards"
        );
        assert_eq!(
            ran(&sharded(3, TestStatus::Timeout)),
            "timed out after 0.3s over 3 runs of 4 shards"
        );
        let rerun = TestOutcome {
            runs: 3,
            ..test(TestStatus::Failed)
        };
        assert_eq!(ran(&rerun), "failed in 0.3s over 3 runs");
    }

    /// The badge is the worst of what a box holds, so a sentence saying less
    /// of the box itself goes on to say what the box holds.
    #[test]
    fn a_box_badged_for_what_it_holds_is_said_to_hold_it_after_what_is_reported_of_it() {
        let fate = Fate {
            target: ConfiguredTarget::new("//gen:gen".to_owned(), None),
            ended: Ended::Built,
        };
        let said = Said {
            saying: Saying::Target(&fate),
            cause: None,
            of: Id(0),
        };
        assert_eq!(
            Badged::Reported(said).words(Status::Succeeded),
            "//gen:gen built"
        );
        assert_eq!(
            Badged::ReportedAndHeld(said).words(Status::RootCause),
            "//gen:gen built; this box holds the root cause of a failure"
        );
        assert_eq!(
            Badged::Held.words(Status::Flaky),
            "this box holds a flaky test"
        );
    }
}
